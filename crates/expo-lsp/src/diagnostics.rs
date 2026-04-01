//! Diagnostics pipeline for the Expo LSP.
//!
//! Handles parsing, type checking, and conversion of Expo compiler
//! diagnostics into LSP diagnostics. When a file belongs to a project
//! (detected by walking up to find `expo.toml`), all sibling project
//! files are parsed and merged into a unified type context so that
//! cross-file type references resolve correctly.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use tower_lsp_server::ls_types::*;

use expo_ast::ast::{Diagnostic as ExpoDiagnostic, Module, Severity as ExpoSeverity};
use expo_typecheck::context::TypeContext;

use crate::backend::{Backend, DocumentState};
use crate::convert::span_to_range;

#[derive(Deserialize)]
struct ExpoToml {
    project: ProjectStub,
    #[serde(default)]
    dependencies: HashMap<String, DepStub>,
}

#[derive(Deserialize)]
struct ProjectStub {
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

/// Parses all project source files (excluding `current_path`) and returns
/// their modules. Also scans local-path dependencies.
fn parse_sibling_modules(project_root: &Path, current_path: Option<&Path>) -> Vec<Module> {
    let toml_path = project_root.join("expo.toml");
    let source = match fs::read_to_string(&toml_path) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let parsed: ExpoToml = match toml::from_str(&source) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };

    let mut modules = Vec::new();

    let scan_roots = |src_dirs: &[String], root: &Path, mods: &mut Vec<Module>| {
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
                            mods.push(pr.module);
                        }
                    }
                }
            }
        }
    };

    scan_roots(&parsed.project.src, project_root, &mut modules);

    for dep in parsed.dependencies.values() {
        if let Some(ref rel) = dep.path {
            let dep_root = project_root.join(rel);
            if let Ok(dep_src) = fs::read_to_string(dep_root.join("expo.toml"))
                && let Ok(dep_toml) = toml::from_str::<ExpoToml>(&dep_src)
            {
                scan_roots(&dep_toml.project.src, &dep_root, &mut modules);
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

/// Extracts a file path from a `file://` URI.
fn uri_to_path(uri: &str) -> Option<PathBuf> {
    if let Some(rest) = uri.strip_prefix("file://") {
        let decoded = percent_decode(rest);
        Some(PathBuf::from(decoded))
    } else {
        None
    }
}

fn percent_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut bytes = s.bytes();
    while let Some(b) = bytes.next() {
        if b == b'%' {
            let hi = bytes.next().and_then(|c| (c as char).to_digit(16));
            let lo = bytes.next().and_then(|c| (c as char).to_digit(16));
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8 as char);
            }
        } else {
            out.push(b as char);
        }
    }
    out
}

impl Backend {
    /// Runs the full diagnostic pipeline on the given source text:
    /// parse, type-check, then publish LSP diagnostics.
    ///
    /// When the file belongs to a project (has an `expo.toml` ancestor),
    /// all sibling project files are parsed so cross-file type references
    /// resolve correctly.
    pub(crate) async fn diagnose(&self, uri: Uri, text: &str, version: Option<i32>) {
        let parse_result = expo_parser::parse(text);

        let mut all_diags: Vec<ExpoDiagnostic> = parse_result.errors;

        let (ctx, project_modules) = if all_diags
            .iter()
            .all(|d| !matches!(d.severity, ExpoSeverity::Error))
        {
            let file_path = uri_to_path(uri.as_str());
            let project_root = file_path
                .as_deref()
                .and_then(|p| p.parent())
                .and_then(find_project_root);

            let sibling_modules = match (&project_root, &file_path) {
                (Some(root), Some(fp)) => parse_sibling_modules(root, Some(fp)),
                _ => Vec::new(),
            };

            let mut all_for_names: Vec<&Module> = self.stdlib_modules.iter().collect();
            for m in &sibling_modules {
                all_for_names.push(m);
            }
            all_for_names.push(&parse_result.module);
            let global_names = expo_typecheck::collect_all_names(&all_for_names);

            let mut unified_ctx = self.stdlib_ctx.clone();
            for m in &sibling_modules {
                let mod_ctx = expo_typecheck::collect_module(m, &global_names);
                unified_ctx.merge(&mod_ctx);
            }

            let mut ctx = expo_typecheck::collect_module(&parse_result.module, &global_names);
            ctx.merge(&unified_ctx);
            expo_typecheck::auto_derive_debug(&mut ctx);
            expo_typecheck::mark_recursive_fields(&mut ctx);
            expo_typecheck::check_module(&parse_result.module, &mut ctx);
            all_diags.extend(ctx.diagnostics.clone());
            (ctx, sibling_modules)
        } else {
            (TypeContext::new(), Vec::new())
        };

        {
            let mut docs = self.documents.write().await;
            docs.insert(
                uri.as_str().to_string(),
                DocumentState {
                    module: parse_result.module,
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
