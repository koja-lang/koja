//! Core LSP backend and server state.
//!
//! Defines the [`Backend`] server, document state management, and the
//! [`LanguageServer`] trait implementation that dispatches to focused
//! handler modules.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::RwLock;
use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;
use tower_lsp_server::{Client, LanguageServer};

use koja_ast::ast::File;
use koja_parser::{ParseMode, ParsedProgram, SourceFile};
use koja_typecheck::{CheckedProgram, GlobalRegistry};

use crate::lookup::LocalIndex;

/// Cached state for a single open document. Holds the parsed program
/// and the optional sealed [`CheckedProgram`] from the typecheck
/// pipeline. On typecheck failure we keep the parsed AST so AST-only
/// handlers (symbols, folding) still work.
pub(crate) struct DocumentState {
    pub(crate) source: String,
    pub(crate) active_path: PathBuf,
    pub(crate) active_package: String,
    pub(crate) parsed: ParsedProgram,
    pub(crate) checked: Option<CheckedProgram>,
    pub(crate) locals: LocalIndex,
}

impl DocumentState {
    /// The currently-edited file, preferring the sealed AST from
    /// `checked` and falling back to the parsed AST when typecheck
    /// failed.
    pub(crate) fn active_file(&self) -> Option<&File> {
        if let Some(checked) = &self.checked {
            for pkg in &checked.packages {
                for file in &pkg.files {
                    if file.path.as_deref() == Some(self.active_path.as_path()) {
                        return Some(file);
                    }
                }
            }
        }
        self.parsed
            .get(&self.active_path)
            .map(|parsed_file| &parsed_file.ast)
    }

    pub(crate) fn registry(&self) -> Option<&GlobalRegistry> {
        self.checked.as_ref().map(|c| &c.registry)
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
    pub fn new(client: Client) -> Self {
        Self {
            client,
            documents: Arc::new(RwLock::new(HashMap::new())),
            autoimport_sources: Arc::new(koja_stdlib::autoimport_sources()),
            qualified_sources: Arc::new(koja_stdlib::qualified_sources()),
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
