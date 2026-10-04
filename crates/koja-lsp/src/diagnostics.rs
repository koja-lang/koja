//! Diagnostics pipeline for the Koja LSP.
//!
//! Loads the active buffer's project through the compiler's
//! [`ProjectLoader`] with the open editor buffers as overlays, runs
//! the pipeline ([`parse_program`] then [`check_program`]), groups
//! parse-phase and check-phase diagnostics by the file that owns
//! them, and publishes each group to its own URI.
//!
//! The load is check-shaped. It takes the manifest's test directories
//! and links the `Test` package, the way `koja check` does, so test
//! files see each other and `assert` resolves.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use tower_lsp_server::ls_types::*;

use koja_ast::ast::{Diagnostic as KojaDiagnostic, Severity as KojaSeverity};
use koja_ast::span::Span;
use koja_parser::{ParseMode, ParsedProgram, SourceFile, SourceTable, parse_program};
use koja_project::{
    Dependencies, ErrorPolicy, LoadOptions, Loaded, LoadedSource, ProjectConfig, ProjectLoader,
    SourceOrigin, StdlibOptions, find_project_root, load_project, stdlib_sources,
};
use koja_query::{Analysis, ReferenceIndex};
use koja_typecheck::{CheckedPackage, CheckedProgram, check_program};

use crate::backend::Backend;
use crate::code_action::FixData;
use crate::convert::{path_to_uri, uri_to_path};
use crate::document::DocumentState;

/// Derives a package name for an LSP-owned file from its on-disk path.
/// Untitled buffers fall back to `"__lsp_preview__"` so every call
/// site passes a real, non-empty package to the type checker.
fn package_for_path(path: Option<&Path>) -> String {
    path.and_then(|p| p.file_stem())
        .and_then(|s| s.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| "__lsp_preview__".to_string())
}

/// The project an open file belongs to, with its manifest loaded.
struct Project {
    config: ProjectConfig,
    root: PathBuf,
}

/// The URIs holding published diagnostics, grouped by the project
/// root they were checked under. A pass diffs against its own
/// project's set alone, so a check in one project never clears
/// another project's diagnostics. Files outside any project share
/// the `None` group.
///
/// The lock is a plain mutex and is never held across an `await`.
#[derive(Debug, Default)]
pub(crate) struct Published(Mutex<HashMap<Option<PathBuf>, HashSet<Uri>>>);

impl Published {
    /// Replace the set for `project` with `now` and return the URIs
    /// that dropped out and need an empty publish. The active URI
    /// always gets its own publish, so it is never stale.
    fn replace(&self, project: Option<PathBuf>, now: HashSet<Uri>, active: &Uri) -> Vec<Uri> {
        let mut published = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let group = published.entry(project).or_default();
        let stale = stale_uris(group, &now, active);
        *group = now;
        stale
    }
}

/// The manifest-bearing project that owns `path`, if any. A manifest
/// that fails to load is reported and treated as no project, so the
/// file still gets single-file diagnostics.
fn project_of(path: &Path) -> Result<Option<Project>, String> {
    let Some(root) = path.parent().and_then(find_project_root) else {
        return Ok(None);
    };
    match load_project(&root) {
        Ok(Some(config)) => Ok(Some(Project { config, root })),
        Ok(None) => Ok(None),
        Err(err) => Err(format!(
            "ignoring {}: {err}",
            root.join("koja.toml").display()
        )),
    }
}

impl Backend {
    /// Runs the pipeline on the buffer text of `uri` and publishes
    /// diagnostics per owning file. The bundle (stdlib + siblings +
    /// active buffer) is parsed and checked from scratch on every
    /// call. We accept that cost for simplicity and revisit only if
    /// real-world latency complains.
    ///
    /// Runs overlap when edits arrive faster than analysis finishes,
    /// and they can finish out of order. A run whose buffer revision
    /// is no longer current publishes nothing, so a slow old run
    /// never overwrites a fresh one.
    pub(crate) async fn diagnose(&self, uri: Uri) {
        let Some(buffer) = self.buffers.snapshot(&uri) else {
            return;
        };

        // Re-materialize the stdlib extraction if pruned, so cached
        // stdlib paths stay valid for navigation.
        let extraction_root = koja_stdlib::extract().ok();

        let active_path =
            uri_to_path(&uri).unwrap_or_else(|| PathBuf::from(format!("<{}>", uri.as_str())));

        let project = match project_of(&active_path) {
            Ok(project) => project,
            Err(warning) => {
                self.client.log_message(MessageType::WARNING, warning).await;
                None
            }
        };
        let overlays = self.open_document_overlays(&active_path, &buffer.text);
        let Loaded { sources, warnings } =
            load_bundle(project.as_ref(), &overlays, extraction_root);
        for warning in warnings {
            self.client.log_message(MessageType::WARNING, warning).await;
        }

        let (active_package, project_paths, project_root) =
            bundle_identity(&sources, &active_path, project);
        let mut sources: Vec<SourceFile> = sources.into_iter().map(into_source_file).collect();
        // The walk finds a project file on disk. An untitled buffer or
        // a file outside any project is appended here.
        if !sources.iter().any(|source| source.path == active_path) {
            sources.push(SourceFile {
                package: active_package.clone(),
                path: active_path.clone(),
                source: buffer.text.clone(),
            });
        }

        let parsed = parse_program(sources, ParseMode::for_path(&active_path));

        let mut all_diags: Vec<KojaDiagnostic> = parsed
            .files
            .values()
            .flat_map(|file| file.diagnostics.iter().cloned())
            .collect();

        // Typecheck drops the source text, so take the table now.
        let sources = parsed.source_table();

        // Both arms keep the post-typecheck ASTs and the registry.
        // Typecheck runs every pass before it reports errors, so the
        // failure path carries the same stamps as the success path
        // and navigation stays available.
        let (parsed_for_state, registry, has_errors) = match check_program(parsed) {
            Ok(checked) => {
                all_diags.extend(checked.diagnostics.iter().cloned());
                let CheckedProgram {
                    packages, registry, ..
                } = checked;
                (
                    parsed_from_packages(packages),
                    Some(Box::new(registry)),
                    false,
                )
            }
            Err(failure) => {
                all_diags.extend(failure.diagnostics);
                (failure.partial, failure.registry, true)
            }
        };

        let grouped = group_by_file(all_diags, &sources, &active_path, &project_paths);

        let index = match &registry {
            Some(registry) => {
                let analysis = Analysis::new(
                    parsed_for_state.iter().map(|parsed_file| &parsed_file.ast),
                    registry,
                    &sources,
                    has_errors,
                );
                ReferenceIndex::build_filtered(&analysis, |file| {
                    file.path
                        .as_deref()
                        .is_some_and(|path| project_paths.contains(path))
                })
            }
            None => ReferenceIndex::default(),
        };

        let state = DocumentState {
            active_path: active_path.clone(),
            active_package,
            encoding: self.encoding(),
            parsed: parsed_for_state,
            registry,
            sources,
            has_errors,
            project_paths,
            project_root,
            index,
        };

        // A newer revision means another run owns the result now.
        if self.buffers.revision(&uri) != Some(buffer.revision) {
            return;
        }

        let (active_diags, publishes) = convert_grouped(&uri, &state, grouped);
        let project_root = state.project_root.clone();
        self.documents.write().await.insert(uri.clone(), state);

        self.publish(
            uri,
            Some(buffer.version),
            project_root,
            active_diags,
            publishes,
        )
        .await;
    }

    /// Publish each file's diagnostics to its own URI and clear the
    /// URIs in the same project that lost theirs since its previous
    /// pass.
    async fn publish(
        &self,
        uri: Uri,
        version: Option<i32>,
        project_root: Option<PathBuf>,
        active_diags: Vec<Diagnostic>,
        publishes: Vec<(Uri, Vec<Diagnostic>)>,
    ) {
        let mut now_published: HashSet<Uri> = HashSet::new();
        if !active_diags.is_empty() {
            now_published.insert(uri.clone());
        }
        now_published.extend(publishes.iter().map(|(uri, _)| uri.clone()));

        let stale = self.published.replace(project_root, now_published, &uri);

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

    /// Canonical path to buffer text for every open document, so the
    /// project compiles from unsaved editor state, not disk. The
    /// active document reads from `active_text`, the snapshot this
    /// run owns, rather than whatever the buffer holds by now.
    fn open_document_overlays(
        &self,
        active_path: &Path,
        active_text: &str,
    ) -> HashMap<PathBuf, String> {
        let mut overlays: HashMap<PathBuf, String> = self
            .buffers
            .open_texts()
            .into_iter()
            .filter_map(|(doc_uri, text)| {
                let canonical = fs::canonicalize(uri_to_path(&doc_uri)?).ok()?;
                Some((canonical, text))
            })
            .collect();
        if let Ok(canonical) = fs::canonicalize(active_path) {
            overlays.insert(canonical, active_text.to_string());
        }
        overlays
    }
}

/// Every source the active file compiles with, in bundle order. With
/// a project, the loader reads the manifest's `src` and `test`
/// directories and whatever dependencies are on disk, each open
/// buffer standing in for its file. Without one, the bundle is the
/// stdlib alone and the caller appends the active buffer.
fn load_bundle(
    project: Option<&Project>,
    overlays: &HashMap<PathBuf, String>,
    extraction_root: Option<PathBuf>,
) -> Loaded {
    let stdlib = || StdlibOptions {
        extraction_root: extraction_root.clone(),
        link_tests: true,
    };
    let stdlib_alone = || Loaded {
        sources: stdlib_sources(&BTreeSet::new(), &stdlib()),
        warnings: Vec::new(),
    };
    let Some(project) = project else {
        return stdlib_alone();
    };
    ProjectLoader::new(&project.config, &project.root)
        .overlays(overlays)
        .sources(LoadOptions {
            dependencies: Dependencies::OnDisk,
            extensions: &["koja"],
            include_tests: true,
            on_error: ErrorPolicy::Lenient,
            stdlib: Some(stdlib()),
        })
        .unwrap_or_else(|err| {
            let mut loaded = stdlib_alone();
            loaded
                .warnings
                .push(format!("{err}, loading the file on its own"));
            loaded
        })
}

/// What the run records about the active file's place in the bundle.
/// The package is the project's namespace, or one derived from the
/// path outside a project. The project paths are every non-stdlib
/// source plus the active file, which the walk may not have found.
fn bundle_identity(
    sources: &[LoadedSource],
    active_path: &Path,
    project: Option<Project>,
) -> (String, HashSet<PathBuf>, Option<PathBuf>) {
    let (active_package, project_root) = match project {
        Some(project) => (project.config.namespace(), Some(project.root)),
        None => (package_for_path(Some(active_path)), None),
    };
    let mut project_paths: HashSet<PathBuf> = sources
        .iter()
        .filter(|source| source.origin != SourceOrigin::Stdlib)
        .map(|source| source.path.clone())
        .collect();
    project_paths.insert(active_path.to_path_buf());
    (active_package, project_paths, project_root)
}

fn into_source_file(loaded: LoadedSource) -> SourceFile {
    SourceFile {
        package: loaded.package,
        path: loaded.path,
        source: loaded.source,
    }
}

/// Convert the grouped diagnostics to LSP form. Returns the active
/// file's set and one set per sibling that has any.
fn convert_grouped(
    uri: &Uri,
    state: &DocumentState,
    mut grouped: HashMap<PathBuf, Vec<KojaDiagnostic>>,
) -> (Vec<Diagnostic>, Vec<(Uri, Vec<Diagnostic>)>) {
    let range = |span: &Span| state.range_of(span);
    let active_diags: Vec<Diagnostic> = grouped
        .remove(&state.active_path)
        .unwrap_or_default()
        .iter()
        .map(|d| to_lsp_diagnostic(d, &state.sources, uri, &range))
        .collect();

    let mut publishes: Vec<(Uri, Vec<Diagnostic>)> = Vec::new();
    for (path, diags) in &grouped {
        let Some(sibling_uri) = path_to_uri(path) else {
            continue;
        };
        let converted = diags
            .iter()
            .map(|d| to_lsp_diagnostic(d, &state.sources, &sibling_uri, &range))
            .collect();
        publishes.push((sibling_uri, converted));
    }
    (active_diags, publishes)
}

/// Regroup the checked packages into a [`ParsedProgram`] so the
/// cached `DocumentState` holds the post-check ASTs in the shape the
/// parser produces. Per-file diagnostics are empty because the check
/// phase already drained them.
fn parsed_from_packages(packages: Vec<CheckedPackage>) -> ParsedProgram {
    use std::collections::BTreeMap;
    let mut files = BTreeMap::new();
    let mut order = Vec::new();
    for pkg in packages {
        for file in pkg.files {
            let path = file
                .path
                .clone()
                .unwrap_or_else(|| PathBuf::from(format!("<{}>", pkg.package)));
            order.push(path.clone());
            files.insert(
                path.clone(),
                koja_parser::ParsedFile {
                    ast: file,
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
/// span's file id through `sources`. Unresolved ids anchor to the
/// active file. Paths outside the bundled project files (stdlib,
/// synthetic markers) are dropped because the user cannot act on
/// them.
fn group_by_file(
    diags: Vec<KojaDiagnostic>,
    sources: &SourceTable,
    active_path: &Path,
    project_paths: &HashSet<PathBuf>,
) -> HashMap<PathBuf, Vec<KojaDiagnostic>> {
    let mut grouped: HashMap<PathBuf, Vec<KojaDiagnostic>> = HashMap::new();
    for diag in diags {
        let owner = match sources.path_of(diag.span.file) {
            None => active_path.to_path_buf(),
            Some(path) if path == active_path || project_paths.contains(path) => path.to_path_buf(),
            Some(_) => continue,
        };
        grouped.entry(owner).or_default().push(diag);
    }
    grouped
}

/// Converts a Koja compiler diagnostic to an LSP diagnostic. A
/// related location becomes `related_information`, falling back to
/// `own_uri` when its file id is not in `sources`. A fix rides in
/// `data` for the code action handler. `range` converts a span over
/// the text of the span's file.
fn to_lsp_diagnostic(
    d: &KojaDiagnostic,
    sources: &SourceTable,
    own_uri: &Uri,
    range: &dyn Fn(&Span) -> Range,
) -> Diagnostic {
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

    let data = d
        .fix
        .as_ref()
        .and_then(|fix| serde_json::to_value(FixData::from_fix(fix, range)).ok());

    let related_information = d.related.as_ref().map(|related| {
        let uri = sources
            .path_of(related.span.file)
            .and_then(path_to_uri)
            .unwrap_or_else(|| own_uri.clone());
        vec![DiagnosticRelatedInformation {
            location: Location {
                uri,
                range: range(&related.span),
            },
            message: related.message.clone(),
        }]
    });

    Diagnostic {
        range: range(&d.span),
        severity: Some(severity),
        source: Some("koja".to_string()),
        message,
        related_information,
        tags,
        data,
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

    use crate::convert::{PositionEncoding, Positions};

    use super::*;

    fn range(span: &Span) -> Range {
        Positions::new(PositionEncoding::Utf16, "").range(span)
    }

    fn diag(file: FileId) -> KojaDiagnostic {
        let span = Span {
            file,
            ..Span::default()
        };
        KojaDiagnostic::error("boom", span)
    }

    /// A table of empty files at `paths`, in id order.
    fn table(paths: &[&str]) -> SourceTable {
        SourceTable::new(paths.iter().map(|path| (PathBuf::from(path), "")))
    }

    #[test]
    fn grouping_buckets_by_owning_file() {
        let active = PathBuf::from("/proj/src/main.koja");
        let sibling = PathBuf::from("/proj/src/util.koja");
        let sources = table(&["/proj/src/main.koja", "/proj/src/util.koja"]);
        let project_paths = HashSet::from([sibling.clone()]);

        let grouped = group_by_file(
            vec![diag(FileId(0)), diag(FileId(1)), diag(FileId(1))],
            &sources,
            &active,
            &project_paths,
        );

        assert_eq!(grouped[&active].len(), 1);
        assert_eq!(grouped[&sibling].len(), 2);
    }

    #[test]
    fn grouping_anchors_unresolved_files_to_active() {
        let active = PathBuf::from("/proj/src/main.koja");
        let grouped = group_by_file(
            vec![diag(FileId::UNKNOWN)],
            &table(&[]),
            &active,
            &HashSet::new(),
        );
        assert_eq!(grouped[&active].len(), 1);
    }

    #[test]
    fn grouping_drops_paths_outside_the_project() {
        let active = PathBuf::from("/proj/src/main.koja");
        let sources = table(&[
            "<Global.io>",
            "/home/u/.koja/stdlib/0.16.0-abcd1234/global/src/io.koja",
        ]);
        let grouped = group_by_file(
            vec![diag(FileId(0)), diag(FileId(1))],
            &sources,
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

    #[test]
    fn published_sets_are_diffed_per_project() {
        let published = Published::default();
        let a_main = Uri::from_str("file:///a/src/main.koja").unwrap();
        let a_util = Uri::from_str("file:///a/src/util.koja").unwrap();
        let b_main = Uri::from_str("file:///b/src/main.koja").unwrap();
        let a = Some(PathBuf::from("/a"));
        let b = Some(PathBuf::from("/b"));

        assert!(
            published
                .replace(a.clone(), HashSet::from([a_util.clone()]), &a_main)
                .is_empty()
        );
        // A pass in project `b` leaves `a`'s published set alone.
        assert!(
            published
                .replace(b, HashSet::from([b_main.clone()]), &b_main)
                .is_empty()
        );
        // The next pass in `a` clears what `a` lost and nothing else.
        assert_eq!(published.replace(a, HashSet::new(), &a_main), vec![a_util]);
    }

    #[test]
    fn related_location_resolves_to_its_own_file() {
        let active = Uri::from_str("file:///proj/src/main.koja").unwrap();
        let sources = table(&["/proj/src/main.koja", "/proj/src/util.koja"]);
        let related_span = Span {
            file: FileId(1),
            ..Span::default()
        };
        let diagnostic = diag(FileId(0)).with_related("previous function definition", related_span);

        let converted = to_lsp_diagnostic(&diagnostic, &sources, &active, &range);
        let related = converted.related_information.expect("related information");
        assert_eq!(related.len(), 1);
        assert_eq!(related[0].message, "previous function definition");
        assert_eq!(
            related[0].location.uri,
            Uri::from_str("file:///proj/src/util.koja").unwrap()
        );
    }

    #[test]
    fn fix_rides_in_data() {
        use koja_ast::ast::Edit;

        let active = Uri::from_str("file:///proj/src/main.koja").unwrap();
        let diagnostic = diag(FileId(0)).with_fix("Drop it", vec![Edit::delete(Span::default())]);

        let converted = to_lsp_diagnostic(&diagnostic, &table(&[]), &active, &range);
        let data: FixData = serde_json::from_value(converted.data.expect("data")).unwrap();
        assert_eq!(data.title, "Drop it");
        assert_eq!(data.edits.len(), 1);
        assert!(
            to_lsp_diagnostic(&diag(FileId(0)), &table(&[]), &active, &range)
                .data
                .is_none()
        );
    }

    #[test]
    fn related_location_with_unresolved_file_stays_in_own_uri() {
        let active = Uri::from_str("file:///proj/src/main.koja").unwrap();
        let diagnostic = diag(FileId(0)).with_related("declared here", Span::default());

        let converted = to_lsp_diagnostic(&diagnostic, &table(&[]), &active, &range);
        let related = converted.related_information.expect("related information");
        assert_eq!(related[0].location.uri, active);
    }
}
