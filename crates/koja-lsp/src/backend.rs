//! Core LSP backend and server state.
//!
//! Defines the [`Backend`] server, document state management, and the
//! [`LanguageServer`] trait implementation that dispatches to focused
//! handler modules.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tokio::sync::RwLock;
use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;
use tower_lsp_server::{Client, LanguageServer};

use koja_ast::ast::File;
use koja_ast::span::{FileId, Span};
use koja_parser::{ParseMode, ParsedProgram, SourceFile};
use koja_query::symbol::symbol_at;
use koja_query::{Analysis, ReferenceIndex, Symbol};
use koja_typecheck::GlobalRegistry;

use crate::buffer::Buffers;
use crate::convert::{PositionEncoding, Positions, path_to_uri, uri_to_path};

/// How long after the last edit to wait before analyzing, so a burst
/// of keystrokes costs one run instead of one per key.
const DEBOUNCE: Duration = Duration::from_millis(150);

/// Cached analysis of a single open document, from its last
/// completed run. The text the editor holds right now lives in
/// [`Buffers`] and may be newer.
///
/// `parsed` holds every file in the bundle as typecheck left it,
/// whether typecheck succeeded or reported errors. `registry` is the
/// registry from the same run, `None` only when parsing failed. The
/// two together back every navigation query, so hover, definition,
/// and references keep working while the program has type errors.
pub(crate) struct DocumentState {
    pub(crate) active_path: PathBuf,
    pub(crate) active_package: String,
    /// The client's character encoding, fixed at `initialize`.
    pub(crate) encoding: PositionEncoding,
    pub(crate) parsed: ParsedProgram,
    pub(crate) registry: Option<Box<GlobalRegistry>>,
    /// File table indexed by `FileId`, in the parse order spans
    /// refer to.
    pub(crate) source_paths: Vec<PathBuf>,
    /// The text of every bundled file as this run saw it, indexed
    /// like `source_paths`. Position conversion needs the line a span
    /// sits on.
    pub(crate) source_texts: Vec<Arc<str>>,
    /// True when the run produced an error diagnostic. Rename
    /// refuses to act on such a program.
    pub(crate) has_errors: bool,
    /// Paths of the user's project files, the active buffer
    /// included. Everything else in the bundle is stdlib.
    pub(crate) project_paths: HashSet<PathBuf>,
    /// The directory holding the project's `koja.toml`, `None` for a
    /// file outside any project.
    pub(crate) project_root: Option<PathBuf>,
    /// Reference index over `project_paths`.
    pub(crate) index: ReferenceIndex,
}

impl DocumentState {
    /// The currently-edited file.
    pub(crate) fn active_file(&self) -> Option<&File> {
        self.parsed
            .get(&self.active_path)
            .map(|parsed_file| &parsed_file.ast)
    }

    /// Query view over the cached program. `None` when parsing
    /// failed and no registry exists.
    pub(crate) fn analysis(&self) -> Option<Analysis<'_>> {
        let registry = self.registry.as_deref()?;
        Some(Analysis::new(
            self.parsed.iter().map(|parsed_file| &parsed_file.ast),
            registry,
            &self.source_paths,
            self.has_errors,
        ))
    }

    pub(crate) fn is_project_file(&self, path: &Path) -> bool {
        self.project_paths.contains(path)
    }

    /// The id of the active file in this run's file table.
    pub(crate) fn active_file_id(&self) -> Option<FileId> {
        self.source_paths
            .iter()
            .position(|path| *path == self.active_path)
            .map(|index| FileId(index as u32))
    }

    /// Position conversion over the text of `file`. An id outside the
    /// table converts over the active file, the same fallback the
    /// diagnostics grouping uses.
    pub(crate) fn positions(&self, file: FileId) -> Positions<'_> {
        let text = self
            .source_texts
            .get(file.0 as usize)
            .or_else(|| {
                self.active_file_id()
                    .and_then(|active| self.source_texts.get(active.0 as usize))
            })
            .map_or("", |text| &**text);
        Positions::new(self.encoding, text)
    }

    /// Position conversion over the active file.
    pub(crate) fn active_positions(&self) -> Positions<'_> {
        self.positions(self.active_file_id().unwrap_or(FileId::UNKNOWN))
    }

    /// The LSP range of a span, over the text of the span's file.
    pub(crate) fn range_of(&self, span: &Span) -> Range {
        self.positions(span.file).range(span)
    }

    /// The Koja line and column, both counted from 1, under an LSP
    /// position in the active file.
    pub(crate) fn line_column(&self, position: Position) -> (u32, u32) {
        self.active_positions().line_column(position)
    }

    /// The symbol under an LSP position in the active file.
    pub(crate) fn symbol_at<'a>(
        &self,
        analysis: &Analysis<'a>,
        position: Position,
    ) -> Option<Symbol<'a>> {
        let file = analysis.file_id(&self.active_path)?;
        let (line, column) = self.line_column(position);
        symbol_at(analysis, &self.index, file, line, column)
    }

    /// The URI of the file a span points into, or `fallback` when
    /// the file id does not resolve.
    pub(crate) fn uri_of(&self, analysis: &Analysis<'_>, file: FileId, fallback: &Uri) -> Uri {
        analysis
            .path_of(file)
            .and_then(path_to_uri)
            .unwrap_or_else(|| fallback.clone())
    }
}

/// What `initialize` settled with the client.
#[derive(Debug, Clone, Copy)]
struct Negotiated {
    encoding: PositionEncoding,
    /// Whether the client lets the server register file watchers.
    watches_files: bool,
}

/// The Koja language server backend.
///
/// Holds shared state (cached stdlib sources, open documents) and the
/// LSP client handle used to push diagnostics and notifications.
/// Every field is shared, so a clone is a handle onto the same
/// server for a spawned task.
///
/// The stdlib bundle is split into autoimport and qualified halves so
/// the diagnostics pipeline can selectively skip the package the user
/// is currently editing. Opening `lib/global/src/foo.koja` must not
/// double-bundle the embedded `Global.*` modules alongside the
/// on-disk siblings. Mirrors the `skip_package` behavior of
/// `bundle_many_with_autoimport` in koja-driver's `pipeline`.
#[derive(Clone)]
pub struct Backend {
    pub(crate) client: Client,
    /// The live text of every open document.
    pub(crate) buffers: Arc<Buffers>,
    /// The last completed analysis of every open document.
    pub(crate) documents: Arc<RwLock<HashMap<String, DocumentState>>>,
    pub(crate) autoimport_sources: Arc<Vec<SourceFile>>,
    pub(crate) qualified_sources: Arc<Vec<SourceFile>>,
    /// URIs holding published diagnostics, for stale clearing.
    pub(crate) published: Arc<RwLock<HashSet<Uri>>>,
    negotiated: Arc<OnceLock<Negotiated>>,
}

impl std::fmt::Debug for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Backend").finish()
    }
}

impl std::fmt::Debug for DocumentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DocumentState").finish()
    }
}

impl Backend {
    /// Creates a new backend, pre-loading the stdlib sources.
    /// The sources are parsed fresh on every diagnostic run. Caching
    /// them as `SourceFile`s avoids re-reading the embedded strings on
    /// every keystroke while keeping each parse independent.
    ///
    /// Stdlib sources carry paths into the on-disk extraction so
    /// go-to-definition lands in real files. If extraction fails, the
    /// synthetic `<Package.module>` markers keep diagnostics working.
    pub fn new(client: Client) -> Self {
        let (autoimport, qualified) = match koja_stdlib::extract() {
            Ok(root) => (
                koja_stdlib::autoimport_sources_at(&root),
                koja_stdlib::qualified_sources_at(&root),
            ),
            Err(_) => (
                koja_stdlib::autoimport_sources(),
                koja_stdlib::qualified_sources(),
            ),
        };
        Self {
            client,
            buffers: Arc::new(Buffers::default()),
            documents: Arc::new(RwLock::new(HashMap::new())),
            autoimport_sources: Arc::new(autoimport),
            qualified_sources: Arc::new(qualified),
            published: Arc::new(RwLock::new(HashSet::new())),
            negotiated: Arc::new(OnceLock::new()),
        }
    }

    /// The character encoding settled at `initialize`. UTF-16 until
    /// then, which only matters for a client that skips the handshake.
    pub(crate) fn encoding(&self) -> PositionEncoding {
        self.negotiated
            .get()
            .map_or(PositionEncoding::Utf16, |negotiated| negotiated.encoding)
    }

    /// Analyze `uri` once the editor has been quiet for [`DEBOUNCE`].
    /// `revision` is the buffer revision that asked for the run. A
    /// newer revision by the time the wait ends means a later change
    /// scheduled its own run, so this one stops.
    fn schedule_diagnose(&self, uri: Uri, revision: u64) {
        let backend = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(DEBOUNCE).await;
            if backend.buffers.revision(uri.as_str()) == Some(revision) {
                backend.diagnose(uri).await;
            }
        });
    }

    /// Analyze every open document again after something outside
    /// them changed, each through its own debounce.
    fn rediagnose_open_documents(&self) {
        for uri in self.buffers.open_uris() {
            let Some(revision) = self.buffers.touch(&uri) else {
                continue;
            };
            if let Ok(uri) = uri.parse::<Uri>() {
                self.schedule_diagnose(uri, revision);
            }
        }
    }

    /// Ask the client to report `.koja` and `koja.toml` changes on
    /// disk, so a sibling edited outside the editor refreshes the
    /// diagnostics of the files that depend on it.
    async fn register_file_watchers(&self) {
        let watchers = ["**/*.koja", "**/koja.toml"]
            .into_iter()
            .map(|pattern| FileSystemWatcher {
                glob_pattern: GlobPattern::String(pattern.to_string()),
                kind: None,
            })
            .collect();
        let options = DidChangeWatchedFilesRegistrationOptions { watchers };
        let registration = Registration {
            id: "koja-lsp/watched-files".to_string(),
            method: "workspace/didChangeWatchedFiles".to_string(),
            register_options: serde_json::to_value(options).ok(),
        };
        if let Err(err) = self.client.register_capability(vec![registration]).await {
            self.client
                .log_message(
                    MessageType::WARNING,
                    format!("koja-lsp could not register file watchers: {err}"),
                )
                .await;
        }
    }
}

impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        let negotiated = Negotiated {
            encoding: PositionEncoding::negotiate(&params.capabilities),
            watches_files: params
                .capabilities
                .workspace
                .as_ref()
                .and_then(|workspace| workspace.did_change_watched_files.as_ref())
                .and_then(|watched| watched.dynamic_registration)
                .unwrap_or(false),
        };
        let _ = self.negotiated.set(negotiated);
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                position_encoding: Some(negotiated.encoding.kind()),
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::INCREMENTAL,
                )),
                document_formatting_provider: Some(OneOf::Left(true)),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                definition_provider: Some(OneOf::Left(true)),
                document_symbol_provider: Some(OneOf::Left(true)),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![".".to_string()]),
                    ..Default::default()
                }),
                signature_help_provider: Some(SignatureHelpOptions {
                    trigger_characters: Some(vec!["(".to_string(), ",".to_string()]),
                    retrigger_characters: None,
                    work_done_progress_options: Default::default(),
                }),
                workspace_symbol_provider: Some(OneOf::Left(true)),
                folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
                references_provider: Some(OneOf::Left(true)),
                document_highlight_provider: Some(OneOf::Left(true)),
                rename_provider: Some(OneOf::Right(RenameOptions {
                    prepare_provider: Some(true),
                    work_done_progress_options: Default::default(),
                })),
                inlay_hint_provider: Some(OneOf::Left(true)),
                code_action_provider: Some(CodeActionProviderCapability::Options(
                    CodeActionOptions {
                        code_action_kinds: Some(vec![CodeActionKind::QUICKFIX]),
                        ..Default::default()
                    },
                )),
                code_lens_provider: Some(CodeLensOptions {
                    resolve_provider: Some(false),
                }),
                ..Default::default()
            },
            server_info: Some(ServerInfo {
                name: "koja-lsp".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "koja-lsp initialized")
            .await;
        if self.negotiated.get().is_some_and(|n| n.watches_files) {
            self.register_file_watchers().await;
        }
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let doc = params.text_document;
        self.buffers.open(doc.uri.as_str(), doc.text, doc.version);
        self.diagnose(doc.uri).await;
    }

    /// The change lands before the first `await` so changes apply in
    /// arrival order. See the `buffer` module.
    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        let revision = self.buffers.change(
            uri.as_str(),
            params.text_document.version,
            params.content_changes,
            self.encoding(),
        );
        if let Some(revision) = revision {
            self.schedule_diagnose(uri, revision);
        }
    }

    /// A save analyzes at once. The touch also retires any debounced
    /// run still waiting, since this one covers it.
    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let uri = params.text_document.uri;
        if self.buffers.touch(uri.as_str()).is_some() {
            self.diagnose(uri).await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        self.buffers.close(uri.as_str());
        self.documents.write().await.remove(uri.as_str());
    }

    /// A change on disk to a file that is not open may change what an
    /// open file sees, so every open document is analyzed again.
    /// Open files are already covered by `didChange`.
    async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        let external = params
            .changes
            .iter()
            .any(|change| !self.buffers.is_open(change.uri.as_str()));
        if external {
            self.rediagnose_open_documents();
        }
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        self.handle_hover(params).await
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        self.handle_goto_definition(params).await
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        self.handle_completion(params).await
    }

    async fn signature_help(&self, params: SignatureHelpParams) -> Result<Option<SignatureHelp>> {
        self.handle_signature_help(params).await
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        self.handle_document_symbol(params).await
    }

    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> Result<Option<WorkspaceSymbolResponse>> {
        self.handle_workspace_symbol(params).await
    }

    async fn folding_range(&self, params: FoldingRangeParams) -> Result<Option<Vec<FoldingRange>>> {
        self.handle_folding_range(params).await
    }

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        self.handle_references(params).await
    }

    async fn document_highlight(
        &self,
        params: DocumentHighlightParams,
    ) -> Result<Option<Vec<DocumentHighlight>>> {
        self.handle_document_highlight(params).await
    }

    async fn prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> Result<Option<PrepareRenameResponse>> {
        self.handle_prepare_rename(params).await
    }

    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        self.handle_rename(params).await
    }

    async fn inlay_hint(&self, params: InlayHintParams) -> Result<Option<Vec<InlayHint>>> {
        self.handle_inlay_hint(params).await
    }

    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        self.handle_code_action(params).await
    }

    async fn code_lens(&self, params: CodeLensParams) -> Result<Option<Vec<CodeLens>>> {
        self.handle_code_lens(params).await
    }

    /// Formats the buffer as the editor holds it, not the last
    /// analyzed text, so a save right after a keystroke formats what
    /// the user sees.
    async fn formatting(&self, params: DocumentFormattingParams) -> Result<Option<Vec<TextEdit>>> {
        let uri = params.text_document.uri;
        let Some(buffer) = self.buffers.snapshot(uri.as_str()) else {
            return Ok(None);
        };
        let path = uri_to_path(uri.as_str()).unwrap_or_default();
        let mode = ParseMode::for_path(&path);

        match koja_fmt::format(&buffer.text, mode) {
            koja_fmt::FormatResult::Ok(formatted) => {
                let line_count = buffer.text.lines().count() as u32;
                Ok(Some(vec![TextEdit {
                    range: Range {
                        start: Position::new(0, 0),
                        end: Position::new(line_count, 0),
                    },
                    new_text: formatted,
                }]))
            }
            koja_fmt::FormatResult::ParseErrors(_) => Ok(None),
        }
    }
}
