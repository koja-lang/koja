//! Diagnostics pipeline for the Koja LSP.
//!
//! Bundles stdlib + project sibling files + the active buffer into a
//! single [`ParsedProgram`], runs the pipeline
//! ([`parse_program`] then [`check_program`]), groups parse-phase and
//! check-phase diagnostics by the file that owns them, and publishes
//! each group to its own URI.
//!
//! When a file belongs to a project (detected by walking up to find
//! `koja.toml`), all sibling project files are bundled so cross-file
//! type references resolve correctly, with open editor buffers
//! overlaying their on-disk contents.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use tower_lsp_server::ls_types::*;

use koja_ast::ast::{Diagnostic as KojaDiagnostic, Severity as KojaSeverity};
use koja_parser::{ParseMode, ParsedProgram, SourceFile, parse_program};
use koja_typecheck::{CheckedProgram, check_program};

use crate::backend::{Backend, DocumentState};
use crate::convert::{path_to_uri, span_to_range, uri_to_path};
use crate::lookup::LocalIndex;

#[derive(Deserialize)]
struct KojaToml {
    project: ProjectStub,
    #[serde(default)]
    dependencies: HashMap<String, DepStub>,
}

#[derive(Deserialize)]
struct ProjectStub {
    name: String,
    #[serde(default)]
    namespace: Option<String>,
    #[serde(default = "default_src")]
    src: Vec<String>,
}

impl ProjectStub {
    /// The PascalCase namespace stamped on the package's files,
    /// mirroring `koja_driver`'s `ProjectConfig::namespace`.
    fn namespace(&self) -> String {
        self.namespace
            .clone()
            .unwrap_or_else(|| koja_parser::derive_namespace(&self.name))
    }
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

/// Walks up from `start` looking for a directory containing `koja.toml`.
fn find_project_root(start: &Path) -> Option<PathBuf> {
    let mut dir = start;
    loop {
        if dir.join("koja.toml").exists() {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

/// Recursively collects all `.koja` files under `dir`.
fn collect_koja_files(dir: &Path) -> Vec<PathBuf> {
    let mut result = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return result,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            result.extend(collect_koja_files(&path));
        } else if path.extension().is_some_and(|ext| ext == "koja") {
            result.push(path);
        }
    }
    result
}

/// Collects sibling project [`SourceFile`]s (excluding `current_path`)
/// with their owning package names. Also scans local-path dependencies.
/// Files open in the editor read from `overlays` instead of disk.
/// Returns an empty vec on any I/O or parse-toml failure so the LSP
/// degrades gracefully rather than dropping diagnostics entirely.
fn collect_sibling_sources(
    project_root: &Path,
    current_path: Option<&Path>,
    overlays: &HashMap<PathBuf, String>,
) -> Vec<SourceFile> {
    let toml_path = project_root.join("koja.toml");
    let source = match fs::read_to_string(&toml_path) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let parsed: KojaToml = match toml::from_str(&source) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };

    let mut files: Vec<SourceFile> = Vec::new();
    let namespace = parsed.project.namespace();
    let mut seen_pkgs: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    seen_pkgs.insert(namespace.clone());
    if namespace != "Global" {
        seen_pkgs.insert("Global".to_string());
    }

    push_package_files(
        &parsed.project.src,
        project_root,
        &namespace,
        current_path,
        overlays,
        &mut files,
    );

    for dep in parsed.dependencies.values() {
        let Some(ref rel) = dep.path else { continue };
        push_dep_files(
            &project_root.join(rel),
            &mut seen_pkgs,
            current_path,
            overlays,
            &mut files,
        );
    }

    // Materialized git dependencies: `koja deps get` copies each
    // pinned package (including transitives) into deps/<Package> with
    // its own koja.toml.
    if let Ok(entries) = fs::read_dir(project_root.join("deps")) {
        for entry in entries.flatten() {
            push_dep_files(
                &entry.path(),
                &mut seen_pkgs,
                current_path,
                overlays,
                &mut files,
            );
        }
    }

    files
}

/// Bundle one dependency directory's sources, keyed by the package
/// name in its own koja.toml. Silently skips unreadable or duplicate
/// packages so the LSP degrades gracefully.
fn push_dep_files(
    dep_root: &Path,
    seen_pkgs: &mut std::collections::BTreeSet<String>,
    current_path: Option<&Path>,
    overlays: &HashMap<PathBuf, String>,
    out: &mut Vec<SourceFile>,
) {
    let Ok(dep_src) = fs::read_to_string(dep_root.join("koja.toml")) else {
        return;
    };
    let Ok(dep_toml) = toml::from_str::<KojaToml>(&dep_src) else {
        return;
    };
    let namespace = dep_toml.project.namespace();
    if !seen_pkgs.insert(namespace.clone()) {
        return;
    }
    push_package_files(
        &dep_toml.project.src,
        dep_root,
        &namespace,
        current_path,
        overlays,
        out,
    );
}

fn push_package_files(
    src_dirs: &[String],
    package_root: &Path,
    package: &str,
    current_path: Option<&Path>,
    overlays: &HashMap<PathBuf, String>,
    out: &mut Vec<SourceFile>,
) {
    for src in src_dirs {
        let dir = package_root.join(src);
        if !dir.is_dir() {
            continue;
        }
        for file_path in collect_koja_files(&dir) {
            if current_path.is_some_and(|cp| same_file(&file_path, cp)) {
                continue;
            }
            let overlay = fs::canonicalize(&file_path)
                .ok()
                .and_then(|canonical| overlays.get(&canonical).cloned());
            let text = match overlay {
                Some(buffer) => buffer,
                None => match fs::read_to_string(&file_path) {
                    Ok(text) => text,
                    Err(_) => continue,
                },
            };
            out.push(SourceFile {
                package: package.to_string(),
                path: file_path,
                source: text,
            });
        }
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => a == b,
    }
}

fn read_project_namespace(project_root: &Path) -> Option<String> {
    let source = fs::read_to_string(project_root.join("koja.toml")).ok()?;
    let parsed: KojaToml = toml::from_str(&source).ok()?;
    Some(parsed.project.namespace())
}

impl Backend {
    /// Runs the pipeline on the current source text and publishes
    /// diagnostics per owning file. The bundle (stdlib + siblings +
    /// active buffer) is parsed and checked from scratch on every
    /// call. We accept that cost for simplicity and revisit only if
    /// real-world latency complains.
    pub(crate) async fn diagnose(&self, uri: Uri, text: &str, version: Option<i32>) {
        // Re-materialize the stdlib extraction if pruned, so cached
        // stdlib paths stay valid for navigation.
        let _ = koja_stdlib::extract();

        let active_path = uri_to_path(uri.as_str())
            .unwrap_or_else(|| PathBuf::from(format!("<{}>", uri.as_str())));

        let project_root = active_path.parent().and_then(find_project_root);
        let active_package = match (&project_root, active_path.as_path()) {
            (Some(root), _) => {
                read_project_namespace(root).unwrap_or_else(|| package_for_path(Some(&active_path)))
            }
            (None, p) => package_for_path(Some(p)),
        };

        let overlays = self.open_document_overlays(uri.as_str()).await;
        let (sources, project_paths) = self.build_bundle(
            &active_package,
            &active_path,
            text,
            project_root.as_deref(),
            &overlays,
        );

        let parsed = parse_program(sources, ParseMode::for_path(&active_path));

        let mut all_diags: Vec<KojaDiagnostic> = parsed
            .files
            .values()
            .flat_map(|file| file.diagnostics.iter().cloned())
            .collect();

        // On typecheck failure keep the partial ParsedProgram so
        // AST-only handlers (symbols, folding) still see something
        // useful. `source_paths` keeps the original parse order that
        // spans' file ids index into.
        let (checked, parsed_for_state, source_paths) = match check_program(parsed) {
            Ok(checked) => {
                all_diags.extend(checked.diagnostics.iter().cloned());
                let rebuilt = rebuild_parsed_from_checked(&checked);
                let source_paths = checked.source_paths.clone();
                (Some(checked), rebuilt, source_paths)
            }
            Err(failure) => {
                all_diags.extend(failure.diagnostics);
                (None, failure.partial, failure.source_paths)
            }
        };

        let grouped = group_by_file(all_diags, &source_paths, &active_path, &project_paths);
        let locals = LocalIndex::build(&parsed_for_state, &active_path);

        {
            let mut docs = self.documents.write().await;
            docs.insert(
                uri.as_str().to_string(),
                DocumentState {
                    source: text.to_string(),
                    active_path: active_path.clone(),
                    active_package,
                    parsed: parsed_for_state,
                    checked,
                    locals,
                },
            );
        }

        self.publish_grouped(uri, version, &active_path, grouped)
            .await;
    }

    /// Publish each file's diagnostics to its own URI and clear the
    /// URIs that lost theirs since the previous pass.
    async fn publish_grouped(
        &self,
        uri: Uri,
        version: Option<i32>,
        active_path: &Path,
        mut grouped: HashMap<PathBuf, Vec<KojaDiagnostic>>,
    ) {
        let active_diags: Vec<Diagnostic> = grouped
            .remove(active_path)
            .unwrap_or_default()
            .iter()
            .map(to_lsp_diagnostic)
            .collect();

        let mut publishes: Vec<(Uri, Vec<Diagnostic>)> = Vec::new();
        let mut now_published: HashSet<Uri> = HashSet::new();
        if !active_diags.is_empty() {
            now_published.insert(uri.clone());
        }
        for (path, diags) in &grouped {
            let Some(sibling_uri) = path_to_uri(path) else {
                continue;
            };
            now_published.insert(sibling_uri.clone());
            publishes.push((sibling_uri, diags.iter().map(to_lsp_diagnostic).collect()));
        }

        let stale: Vec<Uri> = {
            let mut published = self.published.write().await;
            let stale = stale_uris(&published, &now_published, &uri);
            *published = now_published;
            stale
        };

        self.client
            .publish_diagnostics(uri, active_diags, version)
            .await;
        for (sibling_uri, diags) in publishes {
            self.client
                .publish_diagnostics(sibling_uri, diags, None)
                .await;
        }
        for stale_uri in stale {
            self.client
                .publish_diagnostics(stale_uri, Vec::new(), None)
                .await;
        }
    }

    /// Canonical path to buffer text for every other open document,
    /// so siblings compile from unsaved editor state, not disk.
    async fn open_document_overlays(&self, active_uri: &str) -> HashMap<PathBuf, String> {
        let docs = self.documents.read().await;
        docs.iter()
            .filter(|(doc_uri, _)| doc_uri.as_str() != active_uri)
            .filter_map(|(_, state)| {
                let canonical = fs::canonicalize(&state.active_path).ok()?;
                Some((canonical, state.source.clone()))
            })
            .collect()
    }
}

impl Backend {
    /// Bundle the source list for `parse_program`, plus the project
    /// paths eligible for published diagnostics.
    ///
    /// Mirrors [`koja_driver::pipeline::bundle_many_with_autoimport`]: the
    /// embedded autoimport set is dropped for any module already
    /// provided by the active package (so opening
    /// `lib/global/src/debug.koja` doesn't double-define `Global.debug`),
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
        overlays: &HashMap<PathBuf, String>,
    ) -> (Vec<SourceFile>, HashSet<PathBuf>) {
        let mut sources: Vec<SourceFile> =
            Vec::with_capacity(self.autoimport_sources.len() + self.qualified_sources.len() + 4);
        sources.extend(filter_stdlib(&self.autoimport_sources, active_package));
        if active_package != "Global" {
            sources.extend(filter_stdlib(&self.qualified_sources, active_package));
        }
        let mut project_paths = HashSet::new();
        if let Some(root) = project_root {
            for sibling in collect_sibling_sources(root, Some(active_path), overlays) {
                project_paths.insert(sibling.path.clone());
                sources.push(sibling);
            }
        }
        sources.push(SourceFile {
            package: active_package.to_string(),
            path: active_path.to_path_buf(),
            source: text.to_string(),
        });
        (sources, project_paths)
    }
}

/// Clone stdlib sources, dropping any entries owned by `active_package`.
/// Those modules come from the user's on-disk project (or the active
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
                koja_parser::ParsedFile {
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

/// URIs whose diagnostics disappeared this pass and need an empty
/// publish. The active URI always gets its own publish, so skip it.
fn stale_uris(published: &HashSet<Uri>, now_published: &HashSet<Uri>, active: &Uri) -> Vec<Uri> {
    published
        .iter()
        .filter(|old| !now_published.contains(old) && *old != active)
        .cloned()
        .collect()
}

/// Bucket diagnostics by the file that owns them, resolving each
/// span's file id through `source_paths`. Unresolved ids anchor to
/// the active file. Paths outside the bundled project files (stdlib,
/// synthetic markers) are dropped because the user cannot act on
/// them.
fn group_by_file(
    diags: Vec<KojaDiagnostic>,
    source_paths: &[PathBuf],
    active_path: &Path,
    project_paths: &HashSet<PathBuf>,
) -> HashMap<PathBuf, Vec<KojaDiagnostic>> {
    let mut grouped: HashMap<PathBuf, Vec<KojaDiagnostic>> = HashMap::new();
    for diag in diags {
        let owner = match source_paths.get(diag.span.file.0 as usize) {
            None => active_path.to_path_buf(),
            Some(path) if path == active_path || project_paths.contains(path) => path.clone(),
            Some(_) => continue,
        };
        grouped.entry(owner).or_default().push(diag);
    }
    grouped
}

/// Converts a Koja compiler diagnostic to an LSP diagnostic.
fn to_lsp_diagnostic(d: &KojaDiagnostic) -> Diagnostic {
    let severity = match d.severity {
        KojaSeverity::Error => DiagnosticSeverity::ERROR,
        KojaSeverity::Warning => DiagnosticSeverity::WARNING,
        KojaSeverity::Note => DiagnosticSeverity::INFORMATION,
    };

    let message = match &d.hint {
        Some(hint) => format!("{}\n{}", d.message, hint),
        None => d.message.clone(),
    };

    let tags = is_deprecation_warning(d).then(|| vec![DiagnosticTag::DEPRECATED]);

    Diagnostic {
        range: span_to_range(&d.span),
        severity: Some(severity),
        source: Some("koja".to_string()),
        message,
        tags,
        ..Default::default()
    }
}

/// Whether `d` is a use-of-deprecated warning, so editors render the
/// span with strikethrough. Keys off the message shape produced by
/// typecheck's deprecation pass (`pipeline/deprecation.rs`). Keep the
/// two in sync when changing the wording.
fn is_deprecation_warning(d: &KojaDiagnostic) -> bool {
    d.severity == KojaSeverity::Warning && d.message.contains("` is deprecated. ")
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use koja_ast::span::{FileId, Span};

    use super::*;

    fn diag(file: FileId) -> KojaDiagnostic {
        let mut span = Span::default();
        span.file = file;
        KojaDiagnostic::error("boom", span)
    }

    #[test]
    fn grouping_buckets_by_owning_file() {
        let active = PathBuf::from("/proj/src/main.koja");
        let sibling = PathBuf::from("/proj/src/util.koja");
        let source_paths = vec![active.clone(), sibling.clone()];
        let project_paths = HashSet::from([sibling.clone()]);

        let grouped = group_by_file(
            vec![diag(FileId(0)), diag(FileId(1)), diag(FileId(1))],
            &source_paths,
            &active,
            &project_paths,
        );

        assert_eq!(grouped[&active].len(), 1);
        assert_eq!(grouped[&sibling].len(), 2);
    }

    #[test]
    fn grouping_anchors_unresolved_files_to_active() {
        let active = PathBuf::from("/proj/src/main.koja");
        let grouped = group_by_file(vec![diag(FileId::UNKNOWN)], &[], &active, &HashSet::new());
        assert_eq!(grouped[&active].len(), 1);
    }

    #[test]
    fn grouping_drops_paths_outside_the_project() {
        let active = PathBuf::from("/proj/src/main.koja");
        let source_paths = vec![
            PathBuf::from("<Global.io>"),
            PathBuf::from("/home/u/.koja/stdlib/0.16.0-abcd1234/global/src/io.koja"),
        ];
        let grouped = group_by_file(
            vec![diag(FileId(0)), diag(FileId(1))],
            &source_paths,
            &active,
            &HashSet::new(),
        );
        assert!(grouped.is_empty());
    }

    #[test]
    fn stale_set_diff_excludes_survivors_and_active() {
        let active = Uri::from_str("file:///proj/src/main.koja").unwrap();
        let survivor = Uri::from_str("file:///proj/src/util.koja").unwrap();
        let lost = Uri::from_str("file:///proj/src/gone.koja").unwrap();

        let published = HashSet::from([active.clone(), survivor.clone(), lost.clone()]);
        let now_published = HashSet::from([survivor]);

        let stale = stale_uris(&published, &now_published, &active);
        assert_eq!(stale, vec![lost]);
    }
}
