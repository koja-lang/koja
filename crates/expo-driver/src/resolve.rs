//! Source set resolution.
//!
//! Two resolution modes:
//! - **Single-file** ([`resolve_sources`]): parse one entry file into a
//!   single-file source set
//! - **Project** ([`resolve_project_sources`]): uses [`ProjectConfig`] to scan
//!   directories for `.expo` files, building a flat namespace with stdlib
//!   auto-imported

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use expo_ast::ast::Module;

use crate::project::{self, ProjectConfig};

/// A single resolved file: its source text, parsed AST, and any parse errors.
pub struct ResolvedFile {
    pub name: String,
    pub path: PathBuf,
    pub source: String,
    pub ast: Module,
    pub errors: Vec<expo_ast::ast::Diagnostic>,
}

/// All files visible to one build: stdlib + project files + dep packages.
pub struct SourceSet {
    pub entry: String,
    pub files: HashMap<String, ResolvedFile>,
    /// File FQNs in processing order (stdlib first, then project files).
    pub order: Vec<String>,
    /// Package names loaded as dependencies (e.g. "json", "http").
    pub dep_packages: Vec<String>,
    /// When the entry is a PascalCase type name (Process entry mode), this
    /// holds the type name (e.g. `"App"`). `None` for legacy `fn main` mode.
    pub entry_type: Option<String>,
}

// =============================================================================
// Single-file resolution
// =============================================================================

/// Builds a [`SourceSet`] containing just the entry file.
pub fn resolve_sources(entry_path: &Path) -> Result<SourceSet, String> {
    let entry_name = entry_path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or("invalid entry file name")?
        .to_string();

    let source = fs::read_to_string(entry_path)
        .map_err(|e| format!("error reading {}: {e}", entry_path.display()))?;
    let parse_result = expo_parser::parse(&source);
    let mut ast = parse_result.module;
    ast.path = Some(entry_path.to_path_buf());

    let mut sources = SourceSet {
        entry: entry_name.clone(),
        files: HashMap::new(),
        order: Vec::new(),
        dep_packages: Vec::new(),
        entry_type: None,
    };

    sources.order.push(entry_name.clone());
    sources.files.insert(
        entry_name.clone(),
        ResolvedFile {
            name: entry_name,
            path: entry_path.to_path_buf(),
            source,
            ast,
            errors: parse_result.errors,
        },
    );

    Ok(sources)
}

// =============================================================================
// Project-mode resolution
// =============================================================================

/// Builds a [`SourceSet`] for a project with `expo.toml`.
///
/// Scans all `src` directories for `.expo` files and adds them to the source
/// set. Stdlib files are inserted first. No import-following or topological
/// sorting -- all project files form a flat namespace.
pub fn resolve_project_sources(
    config: &ProjectConfig,
    project_root: &Path,
) -> Result<SourceSet, String> {
    let src_roots: Vec<PathBuf> = config.src.iter().map(|s| project_root.join(s)).collect();

    let entry = config
        .entry
        .as_deref()
        .ok_or("expo.toml has no `entry` field; required for build/run/check")?;

    let is_type_entry = config.entry_type_name().is_some();

    let entry_fqn = if is_type_entry {
        format!("{}.src", config.name)
    } else {
        format!("{}.{}", config.name, entry)
    };

    let mut sources = SourceSet {
        entry: entry_fqn.clone(),
        files: HashMap::new(),
        order: Vec::new(),
        dep_packages: Vec::new(),
        entry_type: config.entry_type_name().map(|s| s.to_string()),
    };

    if config.name != "std" {
        insert_stdlib(&mut sources);
    }
    scan_directories(&config.name, &src_roots, &mut sources)?;
    resolve_dependencies(config, project_root, &mut sources)?;

    let project_prefix = format!("{}.", config.name);
    if is_type_entry {
        if !sources
            .order
            .iter()
            .any(|n| n.starts_with(&project_prefix) || n == &config.name)
        {
            return Err("no source files found in src directories".to_string());
        }
        if !sources.files.contains_key(&entry_fqn) {
            let first_project = sources
                .order
                .iter()
                .find(|n| n.starts_with(&project_prefix) || *n == &config.name)
                .cloned();
            if let Some(name) = first_project {
                sources.entry = name;
            }
        }
    } else if !sources.files.contains_key(&entry_fqn) {
        return Err(format!("entry file `{entry}` not found in src directories"));
    }

    Ok(sources)
}

/// Parses all embedded stdlib files and inserts them into the source set.
pub fn insert_stdlib(sources: &mut SourceSet) {
    for &(name, source) in expo_stdlib::SOURCES {
        let parse_result = expo_parser::parse(source);
        sources.order.push(name.to_string());
        sources.files.insert(
            name.to_string(),
            ResolvedFile {
                name: name.to_string(),
                path: PathBuf::from(format!("<{name}>")),
                source: source.to_string(),
                ast: parse_result.module,
                errors: parse_result.errors,
            },
        );
    }
}

/// Scans directories for `.expo` files and adds each as a file to the source
/// set. The fully qualified name is `{project_name}.{relative_path}` where
/// `relative_path` is the file path relative to the src root with `.expo`
/// stripped and `/` replaced by `.`.
fn scan_directories(
    project_name: &str,
    roots: &[PathBuf],
    sources: &mut SourceSet,
) -> Result<(), String> {
    for root in roots {
        if !root.is_dir() {
            continue;
        }
        let files = collect_expo_files_recursive(root);
        for file_path in files {
            let relative_fqn = file_path
                .strip_prefix(root)
                .unwrap_or(&file_path)
                .with_extension("")
                .components()
                .filter_map(|c| c.as_os_str().to_str())
                .collect::<Vec<_>>()
                .join(".");
            let fqn = format!("{project_name}.{relative_fqn}");
            if sources.files.contains_key(&fqn) {
                continue;
            }

            let source = fs::read_to_string(&file_path)
                .map_err(|e| format!("error reading {}: {e}", file_path.display()))?;
            let parse_result = expo_parser::parse(&source);
            let mut ast = parse_result.module;
            ast.path = Some(file_path.clone());

            sources.order.push(fqn.clone());
            sources.files.insert(
                fqn,
                ResolvedFile {
                    name: format!("{project_name}.{relative_fqn}"),
                    path: file_path,
                    source,
                    ast,
                    errors: parse_result.errors,
                },
            );
        }
    }
    Ok(())
}

/// Builds a [`SourceSet`] for running tests.
///
/// Like [`resolve_project_sources`], but also scans `test` directories and
/// includes all source + test files in the source set. The project's entry
/// file (which contains `fn main`) is excluded since the test harness
/// replaces it. The `entry` field is left empty -- the caller inserts a
/// generated harness.
pub fn resolve_test_project_sources(
    config: &ProjectConfig,
    project_root: &Path,
) -> Result<SourceSet, String> {
    let src_roots: Vec<PathBuf> = config.src.iter().map(|s| project_root.join(s)).collect();
    let test_roots: Vec<PathBuf> = config.test.iter().map(|s| project_root.join(s)).collect();

    let all_roots: Vec<PathBuf> = src_roots.iter().chain(test_roots.iter()).cloned().collect();

    let mut sources = SourceSet {
        entry: String::new(),
        files: HashMap::new(),
        order: Vec::new(),
        dep_packages: Vec::new(),
        entry_type: None,
    };

    if config.name != "std" {
        insert_stdlib(&mut sources);
    }
    scan_directories(&config.name, &all_roots, &mut sources)?;
    resolve_dependencies(config, project_root, &mut sources)?;

    if let Some(ref entry) = config.entry
        && config.entry_type_name().is_none()
    {
        let entry_fqn = format!("{}.{}", config.name, entry);
        if let Some(pos) = sources.order.iter().position(|n| n == &entry_fqn) {
            sources.order.remove(pos);
        }
        sources.files.remove(&entry_fqn);
    }

    Ok(sources)
}

/// Scans each dependency declared in `[dependencies]` and adds its source
/// files to the source set. The dep's entry file is skipped to avoid
/// `fn main` conflicts with the consuming project.
///
/// Enforces the duplicate-package-name rule: every project implicitly imports
/// `std`, and no two packages in the dependency graph (project + implicit
/// `std` + each declared dep's `[project] name`) may share a name. The real
/// stdlib (the lone project with `name = "std"`) is the one project that
/// doesn't get the implicit `std` entry, so its self-build does not collide.
fn resolve_dependencies(
    config: &ProjectConfig,
    project_root: &Path,
    sources: &mut SourceSet,
) -> Result<(), String> {
    let mut seen_pkgs: BTreeSet<String> = BTreeSet::new();
    seen_pkgs.insert(config.name.clone());
    if config.name != "std" {
        seen_pkgs.insert("std".to_string());
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
                "duplicate package name `{}` in dependency graph (declared by project, dependency `{alias}`, or implicit `std` import)",
                dep_config.name
            ));
        }

        let dep_src_roots: Vec<PathBuf> = dep_config.src.iter().map(|s| dep_path.join(s)).collect();
        scan_directories(&dep_config.name, &dep_src_roots, sources)?;
        sources.dep_packages.push(dep_config.name.clone());

        if let Some(ref entry) = dep_config.entry {
            let entry_fqn = format!("{}.{}", dep_config.name, entry);
            if let Some(pos) = sources.order.iter().position(|n| n == &entry_fqn) {
                sources.order.remove(pos);
            }
            sources.files.remove(&entry_fqn);
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
