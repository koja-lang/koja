//! Diagnostics pipeline for the Expo LSP.
//!
//! Handles parsing, type checking, and conversion of Expo compiler
//! diagnostics into LSP diagnostics. When a file belongs to a project
//! (detected by walking up to find `expo.toml`), all sibling project
//! files are parsed and merged into a unified type context so that
//! cross-file type references resolve correctly.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use tower_lsp_server::ls_types::*;

use expo_ast::ast::{Diagnostic as ExpoDiagnostic, Module, Severity as ExpoSeverity};
use expo_typecheck::context::TypeContext;
use expo_typecheck::types::{Package, package_for_path, package_from_str};

use crate::backend::{Backend, DocumentState};
use crate::convert::{span_to_range, uri_to_path};

#[derive(Deserialize)]
struct ExpoToml {
    project: ProjectStub,
    #[serde(default)]
    dependencies: HashMap<String, DepStub>,
}

#[derive(Deserialize)]
struct ProjectStub {
    name: String,
    #[serde(default = "default_src")]
    src: Vec<String>,
}

#[derive(Deserialize)]
struct DepStub {
    path: Option<String>,
}

fn default_src() -> Vec<String> {
    vec!["src".to_string()]
}

/// Derives a synthetic package name for an LSP-owned module from its on-disk
/// path. Untitled buffers (no path) fall back to `"__lsp_preview__"` so every
/// call site passes a real, non-empty package to the type checker.
fn package_for_module(path: Option<&Path>) -> String {
    package_for_path(path, "__lsp_preview__")
}

/// Walks up from `start` looking for a directory containing `expo.toml`.
fn find_project_root(start: &Path) -> Option<PathBuf> {
    let mut dir = start;
    loop {
        if dir.join("expo.toml").exists() {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

/// Recursively collects all `.expo` files under `dir`.
fn collect_expo_files(dir: &Path) -> Vec<PathBuf> {
    let mut result = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return result,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            result.extend(collect_expo_files(&path));
        } else if path.extension().is_some_and(|ext| ext == "expo") {
            result.push(path);
        }
    }
    result
}

/// Reads `[project] name` from `<project_root>/expo.toml`, returning `None`
/// if the file is missing, unparseable, or doesn't declare a project name.
fn read_project_name(project_root: &Path) -> Option<String> {
    let source = fs::read_to_string(project_root.join("expo.toml")).ok()?;
    let parsed: ExpoToml = toml::from_str(&source).ok()?;
    Some(parsed.project.name)
}

/// Parses all project source files (excluding `current_path`) and returns
/// each module paired with its owning package name (from the project's
/// `expo.toml`). Also scans local-path dependencies, using each dep's own
/// `[project] name` for its modules. Enforces the duplicate-package-name
/// rule (project + implicit `std` + each dep): on collision, returns the
/// modules collected so far without descending into the offending dep, so
/// the driver-level error eventually surfaces in the editor as well.
fn parse_sibling_modules(
    project_root: &Path,
    current_path: Option<&Path>,
) -> Vec<(Module, String)> {
    let toml_path = project_root.join("expo.toml");
    let source = match fs::read_to_string(&toml_path) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let parsed: ExpoToml = match toml::from_str(&source) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };

    let mut modules: Vec<(Module, String)> = Vec::new();

    let scan_roots =
        |src_dirs: &[String], root: &Path, pkg: &str, mods: &mut Vec<(Module, String)>| {
            for src in src_dirs {
                let dir = root.join(src);
                if dir.is_dir() {
                    for file in collect_expo_files(&dir) {
                        if current_path.is_some_and(|cp| same_file(&file, cp)) {
                            continue;
                        }
                        if let Ok(text) = fs::read_to_string(&file) {
                            let pr = expo_parser::parse(&text);
                            if pr
                                .errors
                                .iter()
                                .all(|d| !matches!(d.severity, ExpoSeverity::Error))
                            {
                                let mut module = pr.module;
                                module.path = Some(file.clone());
                                mods.push((module, pkg.to_string()));
                            }
                        }
                    }
                }
            }
        };

    let project_pkg = parsed.project.name.clone();
    let mut seen_pkgs: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    seen_pkgs.insert(project_pkg.clone());
    if project_pkg != "std" {
        seen_pkgs.insert("std".to_string());
    }

    scan_roots(
        &parsed.project.src,
        project_root,
        &project_pkg,
        &mut modules,
    );

    for dep in parsed.dependencies.values() {
        if let Some(ref rel) = dep.path {
            let dep_root = project_root.join(rel);
            if let Ok(dep_src) = fs::read_to_string(dep_root.join("expo.toml"))
                && let Ok(dep_toml) = toml::from_str::<ExpoToml>(&dep_src)
            {
                let dep_pkg = dep_toml.project.name.clone();
                if !seen_pkgs.insert(dep_pkg.clone()) {
                    // Duplicate package name in dep graph; skip it. The
                    // driver pipeline reports a hard error for this; the LSP
                    // simply omits the offending dep so the rest of the
                    // project still type-checks.
                    continue;
                }
                scan_roots(&dep_toml.project.src, &dep_root, &dep_pkg, &mut modules);
            }
        }
    }

    modules
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => a == b,
    }
}

impl Backend {
    /// Runs the full diagnostic pipeline on the given source text:
    /// parse, type-check, then publish LSP diagnostics.
    ///
    /// When the file belongs to a project (has an `expo.toml` ancestor),
    /// all sibling project files are parsed so cross-file type references
    /// resolve correctly.
    pub(crate) async fn diagnose(&self, uri: Uri, text: &str, version: Option<i32>) {
        let mut parse_result = expo_parser::parse(text);
        let file_path = uri_to_path(uri.as_str());

        let mut all_diags: Vec<ExpoDiagnostic> = parse_result.errors;

        let (ctx, project_modules) = if all_diags
            .iter()
            .all(|d| !matches!(d.severity, ExpoSeverity::Error))
        {
            let project_root = file_path
                .as_deref()
                .and_then(|p| p.parent())
                .and_then(find_project_root);

            let sibling_modules: Vec<(Module, String)> = match (&project_root, &file_path) {
                (Some(root), Some(fp)) => parse_sibling_modules(root, Some(fp)),
                _ => Vec::new(),
            };

            let mut all_for_names: Vec<&Module> = self.stdlib_modules.iter().collect();
            for (m, _) in &sibling_modules {
                all_for_names.push(m);
            }
            all_for_names.push(&parse_result.module);

            let current_pkg = project_root
                .as_deref()
                .and_then(read_project_name)
                .unwrap_or_else(|| package_for_module(file_path.as_deref()));
            let mut known_packages: BTreeSet<Package> = BTreeSet::from([Package::Std]);
            for (_, sibling_pkg) in &sibling_modules {
                known_packages.insert(package_from_str(sibling_pkg));
            }
            known_packages.insert(package_from_str(&current_pkg));
            let global_names = expo_typecheck::collect_all_names(&all_for_names, known_packages);

            let mut unified_ctx = self.stdlib_ctx.clone();
            for (m, sibling_pkg) in &sibling_modules {
                let mod_ctx = expo_typecheck::collect_module(m, &global_names, sibling_pkg);
                unified_ctx.merge(&mod_ctx);
            }

            let mut ctx =
                expo_typecheck::collect_module(&parse_result.module, &global_names, &current_pkg);
            ctx.merge(&unified_ctx);
            expo_typecheck::auto_derive_debug(&mut ctx);
            expo_typecheck::mark_recursive_fields(&mut ctx);
            expo_typecheck::resolve_module_aliases(&parse_result.module, &mut ctx);
            expo_typecheck::resolve_packages(&mut ctx);
            expo_typecheck::check_module(&mut parse_result.module, &mut ctx, &current_pkg);
            all_diags.extend(ctx.diagnostics.clone());
            let stored_modules: Vec<Module> = sibling_modules.into_iter().map(|(m, _)| m).collect();
            (ctx, stored_modules)
        } else {
            (TypeContext::new(), Vec::new())
        };

        {
            let mut module = parse_result.module;
            module.path = file_path;

            if !project_modules.is_empty() {
                let mut pm = self.project_modules.write().await;
                *pm = project_modules.clone();
            }

            let mut docs = self.documents.write().await;
            docs.insert(
                uri.as_str().to_string(),
                DocumentState {
                    module,
                    ctx,
                    source: text.to_string(),
                    project_modules,
                },
            );
        }

        let lsp_diags: Vec<Diagnostic> = all_diags.iter().map(to_lsp_diagnostic).collect();

        self.client
            .publish_diagnostics(uri, lsp_diags, version)
            .await;
    }
}

/// Converts an Expo compiler diagnostic to an LSP diagnostic.
fn to_lsp_diagnostic(d: &ExpoDiagnostic) -> Diagnostic {
    let severity = match d.severity {
        ExpoSeverity::Error => DiagnosticSeverity::ERROR,
        ExpoSeverity::Warning => DiagnosticSeverity::WARNING,
        ExpoSeverity::Note => DiagnosticSeverity::INFORMATION,
    };

    let message = match &d.hint {
        Some(hint) => format!("{}\n{}", d.message, hint),
        None => d.message.clone(),
    };

    Diagnostic {
        range: span_to_range(&d.span),
        severity: Some(severity),
        source: Some("expo".to_string()),
        message,
        ..Default::default()
    }
}
