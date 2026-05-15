//! Diagnostics pipeline for the Expo LSP.
//!
//! Bundles stdlib + project sibling files + the active buffer into a
//! single [`ParsedProgram`], runs the alpha pipeline
//! ([`parse_program`] then [`check_program`]), merges parse-phase and
//! check-phase diagnostics, filters to the active path, and publishes
//! them to the client.
//!
//! When a file belongs to a project (detected by walking up to find
//! `expo.toml`), all sibling project files are bundled so cross-file
//! type references resolve correctly.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use tower_lsp_server::ls_types::*;

use expo_alpha_typecheck::{CheckedProgram, check_program};
use expo_ast::ast::{Diagnostic as ExpoDiagnostic, Severity as ExpoSeverity};
use expo_parser::{ParseMode, ParsedProgram, SourceFile, parse_program};

use crate::backend::{Backend, DocumentState};
use crate::convert::{span_to_range, uri_to_path};
use crate::lookup::LocalIndex;

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

/// Derives a package name for an LSP-owned file from its on-disk path.
/// Untitled buffers fall back to `"__lsp_preview__"` so every call
/// site passes a real, non-empty package to the type checker.
fn package_for_path(path: Option<&Path>) -> String {
    path.and_then(|p| p.file_stem())
        .and_then(|s| s.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| "__lsp_preview__".to_string())
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

/// Collects sibling project [`SourceFile`]s (excluding `current_path`)
/// with their owning package names. Also scans local-path dependencies.
/// Returns an empty vec on any I/O or parse-toml failure so the LSP
/// degrades gracefully rather than dropping diagnostics entirely.
fn collect_sibling_sources(project_root: &Path, current_path: Option<&Path>) -> Vec<SourceFile> {
    let toml_path = project_root.join("expo.toml");
    let source = match fs::read_to_string(&toml_path) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let parsed: ExpoToml = match toml::from_str(&source) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };

    let mut files: Vec<SourceFile> = Vec::new();
    let mut seen_pkgs: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    seen_pkgs.insert(parsed.project.name.clone());
    if parsed.project.name != "Global" {
        seen_pkgs.insert("Global".to_string());
    }

    push_package_files(
        &parsed.project.src,
        project_root,
        &parsed.project.name,
        current_path,
        &mut files,
    );

    for dep in parsed.dependencies.values() {
        let Some(ref rel) = dep.path else { continue };
        let dep_root = project_root.join(rel);
        let Ok(dep_src) = fs::read_to_string(dep_root.join("expo.toml")) else {
            continue;
        };
        let Ok(dep_toml) = toml::from_str::<ExpoToml>(&dep_src) else {
            continue;
        };
        if !seen_pkgs.insert(dep_toml.project.name.clone()) {
            continue;
        }
        push_package_files(
            &dep_toml.project.src,
            &dep_root,
            &dep_toml.project.name,
            current_path,
            &mut files,
        );
    }

    files
}

fn push_package_files(
    src_dirs: &[String],
    package_root: &Path,
    package: &str,
    current_path: Option<&Path>,
    out: &mut Vec<SourceFile>,
) {
    for src in src_dirs {
        let dir = package_root.join(src);
        if !dir.is_dir() {
            continue;
        }
        for file_path in collect_expo_files(&dir) {
            if current_path.is_some_and(|cp| same_file(&file_path, cp)) {
                continue;
            }
            // Mirror [`expo_driver::alpha::push_package_sources`]: files
            // whose stem starts with `alpha_` are alpha-only sources
            // delivered exclusively through the curated autoimport set
            // (their declarations would land out-of-order if pulled
            // from disk — e.g. `alpha_debug_containers` references
            // `Pair`/`Option`/`Result` and must come after `kernel`).
            if is_alpha_only_path(&file_path) {
                continue;
            }
            let Ok(text) = fs::read_to_string(&file_path) else {
                continue;
            };
            out.push(SourceFile {
                package: package.to_string(),
                path: file_path,
                source: text,
            });
        }
    }
}

fn is_alpha_only_path(path: &Path) -> bool {
    path.file_stem()
        .and_then(|s| s.to_str())
        .is_some_and(|stem| stem.starts_with("alpha_"))
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => a == b,
    }
}

fn read_project_name(project_root: &Path) -> Option<String> {
    let source = fs::read_to_string(project_root.join("expo.toml")).ok()?;
    let parsed: ExpoToml = toml::from_str(&source).ok()?;
    Some(parsed.project.name)
}

impl Backend {
    /// Runs the alpha pipeline on the current source text and publishes
    /// LSP diagnostics for the active document.
    ///
    /// The bundle (stdlib + siblings + active buffer) is parsed and
    /// checked from scratch on every call; we accept that cost for
    /// simplicity and revisit only if real-world latency complains.
    pub(crate) async fn diagnose(&self, uri: Uri, text: &str, version: Option<i32>) {
        let active_path = uri_to_path(uri.as_str())
            .unwrap_or_else(|| PathBuf::from(format!("<{}>", uri.as_str())));

        let project_root = active_path.parent().and_then(find_project_root);
        let active_package = match (&project_root, active_path.as_path()) {
            (Some(root), _) => {
                read_project_name(root).unwrap_or_else(|| package_for_path(Some(&active_path)))
            }
            (None, p) => package_for_path(Some(p)),
        };

        let sources =
            self.build_bundle(&active_package, &active_path, text, project_root.as_deref());

        let parsed = parse_program(sources, ParseMode::File);

        let parse_diags = collect_parse_diagnostics(&parsed, &active_path);
        let check_result = check_program(parsed);
        let (checked, mut check_diags) = match check_result {
            Ok(checked) => {
                let diags = filter_diags(&checked.diagnostics, &active_path);
                (Some(checked), diags)
            }
            Err(failure) => {
                let diags = filter_diags(&failure.diagnostics, &active_path);
                // Recover the partial ParsedProgram so AST-only handlers
                // (symbols, folding) still see something useful on
                // typecheck failure.
                let parsed = failure.partial;
                let locals = LocalIndex::build(&parsed, &active_path);
                let mut all_diags = parse_diags.clone();
                all_diags.extend(diags);
                let lsp_diags: Vec<Diagnostic> = all_diags.iter().map(to_lsp_diagnostic).collect();
                {
                    let mut docs = self.documents.write().await;
                    docs.insert(
                        uri.as_str().to_string(),
                        DocumentState {
                            source: text.to_string(),
                            active_path: active_path.clone(),
                            active_package: active_package.clone(),
                            parsed,
                            checked: None,
                            locals,
                        },
                    );
                }
                self.client
                    .publish_diagnostics(uri, lsp_diags, version)
                    .await;
                return;
            }
        };

        let mut all_diags = parse_diags;
        all_diags.append(&mut check_diags);
        let lsp_diags: Vec<Diagnostic> = all_diags.iter().map(to_lsp_diagnostic).collect();

        let parsed_again = rebuild_parsed_from_checked(checked.as_ref().unwrap());
        let locals = LocalIndex::build(&parsed_again, &active_path);

        {
            let mut docs = self.documents.write().await;
            docs.insert(
                uri.as_str().to_string(),
                DocumentState {
                    source: text.to_string(),
                    active_path: active_path.clone(),
                    active_package,
                    parsed: parsed_again,
                    checked,
                    locals,
                },
            );
        }

        self.client
            .publish_diagnostics(uri, lsp_diags, version)
            .await;
    }
}

impl Backend {
    /// Bundle the source list that gets fed to `parse_program`.
    ///
    /// Mirrors [`expo_driver::alpha::bundle_many_with_autoimport`]: the
    /// embedded autoimport set is dropped for any module already
    /// provided by the active package (so opening
    /// `lib/global/src/debug.expo` doesn't double-define `Global.debug`),
    /// and the qualified bundle is skipped entirely when the user is
    /// editing `Global` because the prebaked qualified packages were
    /// typechecked against the published Global and would clash with
    /// the in-progress edits.
    fn build_bundle(
        &self,
        active_package: &str,
        active_path: &Path,
        text: &str,
        project_root: Option<&Path>,
    ) -> Vec<SourceFile> {
        let mut sources: Vec<SourceFile> =
            Vec::with_capacity(self.autoimport_sources.len() + self.qualified_sources.len() + 4);
        sources.extend(filter_stdlib(&self.autoimport_sources, active_package));
        if active_package != "Global" {
            sources.extend(filter_stdlib(&self.qualified_sources, active_package));
        }
        if let Some(root) = project_root {
            sources.extend(collect_sibling_sources(root, Some(active_path)));
        }
        sources.push(SourceFile {
            package: active_package.to_string(),
            path: active_path.to_path_buf(),
            source: text.to_string(),
        });
        sources
    }
}

/// Clone stdlib sources, dropping any entries owned by `active_package`
/// — those modules come from the user's on-disk project (or the active
/// buffer) and a second definition would collide at registry seal time.
fn filter_stdlib(src: &[SourceFile], active_package: &str) -> Vec<SourceFile> {
    src.iter()
        .filter(|s| s.package != active_package)
        .map(|s| SourceFile {
            package: s.package.clone(),
            path: s.path.clone(),
            source: s.source.clone(),
        })
        .collect()
}

/// Build a fresh [`ParsedProgram`] from a sealed [`CheckedProgram`]
/// so the cached `DocumentState` exposes the post-check ASTs to
/// downstream handlers without holding onto the original parsed map.
/// The reconstructed program is `package`/`path`-keyed exactly like
/// the parser's output, with empty per-file diagnostics (the
/// check-phase already drained them).
fn rebuild_parsed_from_checked(checked: &CheckedProgram) -> ParsedProgram {
    use std::collections::BTreeMap;
    let mut files = BTreeMap::new();
    let mut order = Vec::new();
    for pkg in &checked.packages {
        for file in &pkg.files {
            let path = file
                .path
                .clone()
                .unwrap_or_else(|| PathBuf::from(format!("<{}>", pkg.package)));
            order.push(path.clone());
            files.insert(
                path.clone(),
                expo_parser::ParsedFile {
                    ast: file.clone(),
                    diagnostics: Vec::new(),
                    package: pkg.package.clone(),
                    path,
                    source: String::new(),
                },
            );
        }
    }
    ParsedProgram { files, order }
}

fn collect_parse_diagnostics(parsed: &ParsedProgram, active_path: &Path) -> Vec<ExpoDiagnostic> {
    let mut out = Vec::new();
    if let Some(file) = parsed.get(active_path) {
        out.extend(file.diagnostics.iter().cloned());
    }
    out
}

/// Forward all check-phase diagnostics to the active URI. Today's
/// `ExpoDiagnostic` carries only a [`Span`] (no file path), so the
/// LSP can't yet split a multi-file bundle's diagnostics across
/// per-URI streams; users see every check-phase error attributed to
/// whichever file last triggered `diagnose`. Acceptable for v1
/// alongside the big-bang flip; revisit when diagnostics learn to
/// carry their owning path.
fn filter_diags(diags: &[ExpoDiagnostic], _active_path: &Path) -> Vec<ExpoDiagnostic> {
    diags.to_vec()
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
