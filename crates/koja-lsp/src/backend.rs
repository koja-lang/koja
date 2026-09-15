//! Core LSP backend and server state.
//!
//! Defines the [`Backend`] server, document state management, and the
//! [`LanguageServer`] trait implementation that dispatches to focused
//! handler modules.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::RwLock;
use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;
use tower_lsp_server::{Client, LanguageServer};

use koja_ast::ast::File;
use koja_ast::span::FileId;
use koja_parser::{ParseMode, ParsedProgram, SourceFile};
use koja_query::symbol::symbol_at;
use koja_query::{Analysis, ReferenceIndex, Symbol};
use koja_typecheck::GlobalRegistry;

use crate::convert::path_to_uri;

/// Cached state for a single open document.
///
/// `parsed` holds every file in the bundle as typecheck left it,
/// whether typecheck succeeded or reported errors. `registry` is the
/// registry from the same run, `None` only when parsing failed. The
/// two together back every navigation query, so hover, definition,
/// and references keep working while the program has type errors.
pub(crate) struct DocumentState {
    pub(crate) source: String,
    pub(crate) active_path: PathBuf,
    pub(crate) active_package: String,
    pub(crate) parsed: ParsedProgram,
    pub(crate) registry: Option<Box<GlobalRegistry>>,
    /// File table indexed by `FileId`, in the parse order spans
    /// refer to.
    pub(crate) source_paths: Vec<PathBuf>,
    /// True when the run produced an error diagnostic. Rename
    /// refuses to act on such a program.
    pub(crate) has_errors: bool,
    /// Paths of the user's project files, the active buffer
    /// included. Everything else in the bundle is stdlib.
    pub(crate) project_paths: HashSet<PathBuf>,
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

    /// The symbol under an LSP (0-indexed) position in the active
    /// file.
    pub(crate) fn symbol_at<'a>(
        &self,
        analysis: &Analysis<'a>,
        position: Position,
    ) -> Option<Symbol<'a>> {
        let file = analysis.file_id(&self.active_path)?;
        symbol_at(
            analysis,
            &self.index,
            file,
            position.line + 1,
            position.character + 1,
        )
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

/// The Koja language server backend.
///
/// Holds shared state (cached stdlib sources, open documents) and the
/// LSP client handle used to push diagnostics and notifications.
///
/// The stdlib bundle is split into autoimport and qualified halves so
/// the diagnostics pipeline can selectively skip the package the user
/// is currently editing. Opening `lib/global/src/foo.koja` must not
/// double-bundle the embedded `Global.*` modules alongside the
/// on-disk siblings. Mirrors
/// [`koja_driver::pipeline::bundle_many_with_autoimport`]'s
/// `skip_package` behavior.
pub struct Backend {
    pub(crate) client: Client,
    pub(crate) documents: Arc<RwLock<HashMap<String, DocumentState>>>,
    pub(crate) autoimport_sources: Arc<Vec<SourceFile>>,
    pub(crate) qualified_sources: Arc<Vec<SourceFile>>,
    /// URIs holding published diagnostics, for stale clearing.
    pub(crate) published: Arc<RwLock<HashSet<Uri>>>,
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
            documents: Arc::new(RwLock::new(HashMap::new())),
            autoimport_sources: Arc::new(autoimport),
            qualified_sources: Arc::new(qualified),
            published: Arc::new(RwLock::new(HashSet::new())),
        }
    }
}

impl LanguageServer for Backend {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
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
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let doc = params.text_document;
        self.diagnose(doc.uri, &doc.text, Some(doc.version)).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        if let Some(change) = params.content_changes.into_iter().last() {
            self.diagnose(
                params.text_document.uri,
                &change.text,
                Some(params.text_document.version),
            )
            .await;
        }
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        if let Some(text) = params.text {
            self.diagnose(params.text_document.uri, &text, None).await;
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

    async fn formatting(&self, params: DocumentFormattingParams) -> Result<Option<Vec<TextEdit>>> {
        let uri = params.text_document.uri;

        let docs = self.documents.read().await;
        let state = match docs.get(uri.as_str()) {
            Some(s) => s,
            None => return Ok(None),
        };
        let source = &state.source;
        let mode = ParseMode::for_path(&state.active_path);

        match koja_fmt::format(source, mode) {
            koja_fmt::FormatResult::Ok(formatted) => {
                let line_count = source.lines().count() as u32;
                let last_line_len = source.lines().last().map_or(0, |l| l.len() as u32);

                Ok(Some(vec![TextEdit {
                    range: Range {
                        start: Position::new(0, 0),
                        end: Position::new(line_count, last_line_len),
                    },
                    new_text: formatted,
                }]))
            }
            koja_fmt::FormatResult::ParseErrors(_) => Ok(None),
        }
    }
}
