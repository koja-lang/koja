//! Module graph resolution.
//!
//! Two resolution modes:
//! - **Single-file** ([`resolve_modules`]): parse one entry file into a single-module graph
//! - **Project** ([`resolve_project_modules`]): uses [`ProjectConfig`] to scan directories
//!   for `.expo` files, building a flat namespace with stdlib auto-imported

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use expo_ast::ast::Module;

use crate::project::{self, ProjectConfig};

/// A single resolved module: its source text, parsed AST, and any parse errors.
pub struct ResolvedModule {
    pub name: String,
    pub path: PathBuf,
    pub source: String,
    pub module: Module,
    pub errors: Vec<expo_ast::ast::Diagnostic>,
}

/// All modules in a compilation unit: stdlib + project files.
pub struct ModuleGraph {
    pub entry: String,
    pub modules: HashMap<String, ResolvedModule>,
    /// Module names in processing order (stdlib first, then project files).
    pub order: Vec<String>,
}

// =============================================================================
// Single-file resolution
// =============================================================================

/// Builds a [`ModuleGraph`] containing just the entry file.
pub fn resolve_modules(entry_path: &Path) -> Result<ModuleGraph, String> {
    let entry_name = entry_path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or("invalid entry file name")?
        .to_string();

    let source = fs::read_to_string(entry_path)
        .map_err(|e| format!("error reading {}: {e}", entry_path.display()))?;
    let parse_result = expo_parser::parse(&source);
    let mut module = parse_result.module;
    module.path = Some(entry_path.to_path_buf());

    let mut graph = ModuleGraph {
        entry: entry_name.clone(),
        modules: HashMap::new(),
        order: Vec::new(),
    };

    graph.order.push(entry_name.clone());
    graph.modules.insert(
        entry_name.clone(),
        ResolvedModule {
            name: entry_name,
            path: entry_path.to_path_buf(),
            source,
            module,
            errors: parse_result.errors,
        },
    );

    Ok(graph)
}

// =============================================================================
// Project-mode resolution
// =============================================================================

/// Builds a [`ModuleGraph`] for a project with `expo.toml`.
///
/// Scans all `src` directories for `.expo` files and adds them to the graph.
/// Stdlib modules are inserted first. No import-following or topological
/// sorting -- all project files form a flat namespace.
pub fn resolve_project_modules(
    config: &ProjectConfig,
    project_root: &Path,
) -> Result<ModuleGraph, String> {
    let src_roots: Vec<PathBuf> = config.src.iter().map(|s| project_root.join(s)).collect();

    let entry = config
        .entry
        .as_deref()
        .ok_or("expo.toml has no `entry` field; required for build/run/check")?;

    let entry_fqn = format!("{}.{}", config.name, entry);

    let mut graph = ModuleGraph {
        entry: entry_fqn.clone(),
        modules: HashMap::new(),
        order: Vec::new(),
    };

    insert_stdlib(&mut graph);
    scan_directories(&config.name, &src_roots, &mut graph)?;
    resolve_dependencies(config, project_root, &mut graph)?;

    if !graph.modules.contains_key(&entry_fqn) {
        return Err(format!(
            "entry module `{entry}` not found in src directories"
        ));
    }

    Ok(graph)
}

/// Parses all embedded stdlib modules and inserts them into the graph.
pub fn insert_stdlib(graph: &mut ModuleGraph) {
    for &(name, source) in expo_stdlib::SOURCES {
        let parse_result = expo_parser::parse(source);
        graph.order.push(name.to_string());
        graph.modules.insert(
            name.to_string(),
            ResolvedModule {
                name: name.to_string(),
                path: PathBuf::from(format!("<{name}>")),
                source: source.to_string(),
                module: parse_result.module,
                errors: parse_result.errors,
            },
        );
    }
}

/// Scans directories for `.expo` files and adds each as a module to the graph.
/// The fully qualified name is `{project_name}.{relative_path}` where
/// `relative_path` is the file path relative to the src root with `.expo`
/// stripped and `/` replaced by `.`.
fn scan_directories(
    project_name: &str,
    roots: &[PathBuf],
    graph: &mut ModuleGraph,
) -> Result<(), String> {
    for root in roots {
        if !root.is_dir() {
            continue;
        }
        let files = collect_expo_files_recursive(root);
        for file_path in files {
            let relative_module = file_path
                .strip_prefix(root)
                .unwrap_or(&file_path)
                .with_extension("")
                .components()
                .filter_map(|c| c.as_os_str().to_str())
                .collect::<Vec<_>>()
                .join(".");
            let fqn = format!("{project_name}.{relative_module}");
            if graph.modules.contains_key(&fqn) {
                continue;
            }

            let source = fs::read_to_string(&file_path)
                .map_err(|e| format!("error reading {}: {e}", file_path.display()))?;
            let parse_result = expo_parser::parse(&source);
            let mut module = parse_result.module;
            module.path = Some(file_path.clone());

            graph.order.push(fqn.clone());
            graph.modules.insert(
                fqn,
                ResolvedModule {
                    name: format!("{project_name}.{relative_module}"),
                    path: file_path,
                    source,
                    module,
                    errors: parse_result.errors,
                },
            );
        }
    }
    Ok(())
}

/// Builds a [`ModuleGraph`] for running tests.
///
/// Like [`resolve_project_modules`], but also scans `test` directories and
/// includes all source + test files in the graph. The project's entry module
/// (which contains `fn main`) is excluded since the test harness replaces it.
/// The `entry` field is left empty -- the caller inserts a generated harness.
pub fn resolve_test_project_modules(
    config: &ProjectConfig,
    project_root: &Path,
) -> Result<ModuleGraph, String> {
    let src_roots: Vec<PathBuf> = config.src.iter().map(|s| project_root.join(s)).collect();
    let test_roots: Vec<PathBuf> = config.test.iter().map(|s| project_root.join(s)).collect();

    let all_roots: Vec<PathBuf> = src_roots.iter().chain(test_roots.iter()).cloned().collect();

    let mut graph = ModuleGraph {
        entry: String::new(),
        modules: HashMap::new(),
        order: Vec::new(),
    };

    insert_stdlib(&mut graph);
    scan_directories(&config.name, &all_roots, &mut graph)?;
    resolve_dependencies(config, project_root, &mut graph)?;

    if let Some(ref entry) = config.entry {
        let entry_fqn = format!("{}.{}", config.name, entry);
        if let Some(pos) = graph.order.iter().position(|n| n == &entry_fqn) {
            graph.order.remove(pos);
        }
        graph.modules.remove(&entry_fqn);
    }

    Ok(graph)
}

/// Scans each dependency declared in `[dependencies]` and adds its source
/// modules to the graph. The dep's entry module is skipped to avoid `fn main`
/// conflicts with the consuming project.
fn resolve_dependencies(
    config: &ProjectConfig,
    project_root: &Path,
    graph: &mut ModuleGraph,
) -> Result<(), String> {
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

        let dep_src_roots: Vec<PathBuf> = dep_config.src.iter().map(|s| dep_path.join(s)).collect();
        scan_directories(&dep_config.name, &dep_src_roots, graph)?;

        if let Some(ref entry) = dep_config.entry {
            let entry_fqn = format!("{}.{}", dep_config.name, entry);
            if let Some(pos) = graph.order.iter().position(|n| n == &entry_fqn) {
                graph.order.remove(pos);
            }
            graph.modules.remove(&entry_fqn);
        }
    }
    Ok(())
}

fn collect_expo_files_recursive(dir: &Path) -> Vec<PathBuf> {
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
