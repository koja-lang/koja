//! Module graph resolution and import handling.
//!
//! Starting from an entry file, builds a [`ModuleGraph`] by recursively
//! following `import` statements. The graph is topologically sorted so
//! that dependencies are type-checked before their dependents.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use expo_ast::ast::{ImportTarget, Item, Module};

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

/// DFS traversal that resolves one module and all its transitive imports,
/// detecting circular dependencies via the `visiting` stack.
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

/// Maps a dotted module name to a file path, trying `<name>.expo` first
/// then `<name>/mod.expo`.
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

/// Extracts all import declarations from a parsed module, returning the
/// module name to resolve and the import target kind.
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

/// Returns true if the dotted module name corresponds to an existing file.
fn can_resolve(module_name: &str, root_dir: &Path) -> bool {
    let relative = module_name.replace('.', "/");
    root_dir.join(format!("{relative}.expo")).exists()
        || root_dir.join(&relative).join("mod.expo").exists()
}
