//! Module graph resolution and import handling.
//!
//! Starting from an entry file, builds a [`ModuleGraph`] by recursively
//! following `import` statements. The graph is topologically sorted so
//! that dependencies are type-checked before their dependents.
//!
//! Two resolution modes:
//! - **Single-file** ([`resolve_modules`]): legacy mode, root dir = entry file's parent
//! - **Project** ([`resolve_project_modules`]): uses [`ProjectConfig`] for namespaced
//!   resolution with stdlib auto-imported from embedded sources

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use expo_ast::ast::{ImportTarget, Item, Module};

use crate::project::ProjectConfig;

/// A single resolved module: its source text, parsed AST, and any parse errors.
pub struct ResolvedModule {
    pub name: String,
    pub path: PathBuf,
    pub source: String,
    pub module: Module,
    pub errors: Vec<expo_ast::ast::Diagnostic>,
}

/// The complete set of modules reachable from an entry file, in topological order.
pub struct ModuleGraph {
    pub entry: String,
    pub modules: HashMap<String, ResolvedModule>,
    /// Module names in dependency order (leaves first).
    pub order: Vec<String>,
}

// =============================================================================
// Single-file resolution (existing behavior, unchanged)
// =============================================================================

/// Builds a [`ModuleGraph`] by recursively resolving imports from the entry file.
/// Returns an error if a circular import is detected or a module file cannot be found.
pub fn resolve_modules(entry_path: &Path) -> Result<ModuleGraph, String> {
    let root_dir = entry_path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let entry_name = entry_path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or("invalid entry file name")?
        .to_string();

    let mut graph = ModuleGraph {
        entry: entry_name.clone(),
        modules: HashMap::new(),
        order: Vec::new(),
    };

    let mut visiting: Vec<String> = Vec::new();
    let mut visited: HashSet<String> = HashSet::new();

    resolve_recursive(
        &entry_name,
        entry_path,
        &root_dir,
        &mut graph,
        &mut visiting,
        &mut visited,
    )?;

    Ok(graph)
}

fn resolve_recursive(
    module_name: &str,
    module_path: &Path,
    root_dir: &Path,
    graph: &mut ModuleGraph,
    visiting: &mut Vec<String>,
    visited: &mut HashSet<String>,
) -> Result<(), String> {
    if visited.contains(module_name) {
        return Ok(());
    }

    if visiting.contains(&module_name.to_string()) {
        let cycle_start = visiting.iter().position(|n| n == module_name).unwrap_or(0);
        let cycle: Vec<&str> = visiting[cycle_start..]
            .iter()
            .map(|s| s.as_str())
            .chain(std::iter::once(module_name))
            .collect();
        return Err(format!("circular import detected: {}", cycle.join(" -> ")));
    }

    let source = fs::read_to_string(module_path)
        .map_err(|e| format!("error reading {}: {e}", module_path.display()))?;
    let parse_result = expo_parser::parse(&source);

    visiting.push(module_name.to_string());

    let imports = extract_imports(&parse_result.module, root_dir);
    for (import_module, _) in &imports {
        if graph.modules.contains_key(import_module) || visited.contains(import_module) {
            continue;
        }

        let import_path = resolve_import_path(import_module, root_dir)?;
        resolve_recursive(
            import_module,
            &import_path,
            root_dir,
            graph,
            visiting,
            visited,
        )?;
    }

    visiting.pop();
    visited.insert(module_name.to_string());

    graph.order.push(module_name.to_string());
    graph.modules.insert(
        module_name.to_string(),
        ResolvedModule {
            name: module_name.to_string(),
            path: module_path.to_path_buf(),
            source,
            module: parse_result.module,
            errors: parse_result.errors,
        },
    );

    Ok(())
}

fn resolve_import_path(module_name: &str, root_dir: &Path) -> Result<PathBuf, String> {
    let relative = module_name.replace('.', "/");

    let file_path = root_dir.join(format!("{relative}.expo"));
    if file_path.exists() {
        return Ok(file_path);
    }

    let mod_path = root_dir.join(&relative).join("mod.expo");
    if mod_path.exists() {
        return Ok(mod_path);
    }

    Err(format!(
        "cannot find module `{module_name}`: tried `{relative}.expo` and `{relative}/mod.expo`"
    ))
}

fn extract_imports(module: &Module, root_dir: &Path) -> Vec<(String, ImportTarget)> {
    module
        .items
        .iter()
        .filter_map(|item| {
            if let Item::Import(import) = item {
                match &import.target {
                    ImportTarget::Module | ImportTarget::Wildcard | ImportTarget::Group(_) => {
                        Some((import.path.join("."), import.target.clone()))
                    }
                    ImportTarget::Item(item_name) => {
                        let full_path: Vec<String> = import
                            .path
                            .iter()
                            .cloned()
                            .chain(std::iter::once(item_name.clone()))
                            .collect();
                        let full_module = full_path.join(".");
                        if can_resolve(&full_module, root_dir) {
                            Some((full_module, ImportTarget::Module))
                        } else {
                            Some((import.path.join("."), import.target.clone()))
                        }
                    }
                }
            } else {
                None
            }
        })
        .collect()
}

fn can_resolve(module_name: &str, root_dir: &Path) -> bool {
    let relative = module_name.replace('.', "/");
    root_dir.join(format!("{relative}.expo")).exists()
        || root_dir.join(&relative).join("mod.expo").exists()
}

// =============================================================================
// Project-mode resolution
// =============================================================================

/// Builds a [`ModuleGraph`] for a project with `expo.toml`.
///
/// All stdlib modules are auto-imported (inserted into the graph first, in
/// dependency order). The entry module and its transitive imports are then
/// resolved with namespace-aware dispatch:
///
/// - Project name prefix → strip it, resolve in `src/` dirs
/// - `std` prefix → already in graph from auto-import
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

    let entry_path = resolve_project_import_path(entry, &src_roots)?;

    let mut visiting: Vec<String> = Vec::new();
    let mut visited: HashSet<String> = graph.modules.keys().cloned().collect();

    resolve_project_recursive(
        &entry_fqn,
        &entry_path,
        config,
        &src_roots,
        &mut graph,
        &mut visiting,
        &mut visited,
    )?;

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

fn resolve_project_recursive(
    module_fqn: &str,
    module_path: &Path,
    config: &ProjectConfig,
    src_roots: &[PathBuf],
    graph: &mut ModuleGraph,
    visiting: &mut Vec<String>,
    visited: &mut HashSet<String>,
) -> Result<(), String> {
    if visited.contains(module_fqn) {
        return Ok(());
    }

    if visiting.contains(&module_fqn.to_string()) {
        let cycle_start = visiting.iter().position(|n| n == module_fqn).unwrap_or(0);
        let cycle: Vec<&str> = visiting[cycle_start..]
            .iter()
            .map(|s| s.as_str())
            .chain(std::iter::once(module_fqn))
            .collect();
        return Err(format!("circular import detected: {}", cycle.join(" -> ")));
    }

    let source = fs::read_to_string(module_path)
        .map_err(|e| format!("error reading {}: {e}", module_path.display()))?;
    let parse_result = expo_parser::parse(&source);

    visiting.push(module_fqn.to_string());

    let imports = extract_project_imports(&parse_result.module, config, src_roots);
    for (import_fqn, _) in &imports {
        if graph.modules.contains_key(import_fqn) || visited.contains(import_fqn) {
            continue;
        }

        let relative = strip_project_prefix(import_fqn, &config.name)
            .ok_or_else(|| format!("cannot resolve import `{import_fqn}`: unknown namespace"))?;

        let import_path = resolve_project_import_path(relative, src_roots)?;
        resolve_project_recursive(
            import_fqn,
            &import_path,
            config,
            src_roots,
            graph,
            visiting,
            visited,
        )?;
    }

    visiting.pop();
    visited.insert(module_fqn.to_string());

    graph.order.push(module_fqn.to_string());
    graph.modules.insert(
        module_fqn.to_string(),
        ResolvedModule {
            name: module_fqn.to_string(),
            path: module_path.to_path_buf(),
            source,
            module: parse_result.module,
            errors: parse_result.errors,
        },
    );

    Ok(())
}

/// Strips the project name prefix from a fully qualified module name.
/// Returns `None` if the name doesn't start with the project name.
fn strip_project_prefix<'a>(fqn: &'a str, project_name: &str) -> Option<&'a str> {
    fqn.strip_prefix(project_name)
        .and_then(|rest| rest.strip_prefix('.'))
}

/// Resolves a project-relative module path (with project prefix already stripped)
/// against the src root directories.
fn resolve_project_import_path(
    relative_module: &str,
    src_roots: &[PathBuf],
) -> Result<PathBuf, String> {
    let relative = relative_module.replace('.', "/");

    for root in src_roots {
        let file_path = root.join(format!("{relative}.expo"));
        if file_path.exists() {
            return Ok(file_path);
        }

        let mod_path = root.join(&relative).join("mod.expo");
        if mod_path.exists() {
            return Ok(mod_path);
        }
    }

    Err(format!(
        "cannot find module `{relative_module}`: tried `{relative}.expo` and `{relative}/mod.expo` in src directories"
    ))
}

/// Extracts imports from a module in project mode, returning fully qualified
/// module names. Stdlib imports (prefixed with `std.`) are skipped since
/// they're auto-imported.
fn extract_project_imports(
    module: &Module,
    config: &ProjectConfig,
    src_roots: &[PathBuf],
) -> Vec<(String, ImportTarget)> {
    module
        .items
        .iter()
        .filter_map(|item| {
            if let Item::Import(import) = item {
                let dotted = import.path.join(".");

                if dotted.starts_with("std.") || dotted == "std" {
                    return None;
                }

                match &import.target {
                    ImportTarget::Module | ImportTarget::Wildcard | ImportTarget::Group(_) => {
                        Some((dotted, import.target.clone()))
                    }
                    ImportTarget::Item(item_name) => {
                        let full_path: Vec<String> = import
                            .path
                            .iter()
                            .cloned()
                            .chain(std::iter::once(item_name.clone()))
                            .collect();
                        let full_module = full_path.join(".");
                        if can_resolve_project(&full_module, &config.name, src_roots) {
                            Some((full_module, ImportTarget::Module))
                        } else {
                            Some((dotted, import.target.clone()))
                        }
                    }
                }
            } else {
                None
            }
        })
        .collect()
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

    let entry_fqn = config
        .entry
        .as_ref()
        .map(|e| format!("{}.{}", config.name, e));

    let mut graph = ModuleGraph {
        entry: String::new(),
        modules: HashMap::new(),
        order: Vec::new(),
    };

    insert_stdlib(&mut graph);

    let mut visiting: Vec<String> = Vec::new();
    let mut visited: HashSet<String> = graph.modules.keys().cloned().collect();

    if let Some(ref skip) = entry_fqn {
        visited.insert(skip.clone());
    }

    for root in &all_roots {
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
            let fqn = format!("{}.{}", config.name, relative_module);
            if visited.contains(&fqn) {
                continue;
            }
            resolve_project_recursive(
                &fqn,
                &file_path,
                config,
                &all_roots,
                &mut graph,
                &mut visiting,
                &mut visited,
            )?;
        }
    }

    Ok(graph)
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

/// Checks if a fully qualified module name resolves to a file in project mode.
fn can_resolve_project(fqn: &str, project_name: &str, src_roots: &[PathBuf]) -> bool {
    let Some(relative_module) = strip_project_prefix(fqn, project_name) else {
        return false;
    };
    let relative = relative_module.replace('.', "/");
    src_roots.iter().any(|root| {
        root.join(format!("{relative}.expo")).exists()
            || root.join(&relative).join("mod.expo").exists()
    })
}
