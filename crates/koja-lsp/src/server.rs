//! The [`LanguageServer`] implementation. Holds the capabilities
//! table and the lifecycle notifications, and forwards each request
//! to its handler module. Rust allows one impl block per trait, so
//! the forwards live here together.

use tower_lsp_server::LanguageServer;
use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;

use koja_parser::ParseMode;

use crate::backend::{Backend, Negotiated};
use crate::convert::{PositionEncoding, uri_to_path};

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
        self.buffers.open(doc.uri.clone(), doc.text, doc.version);
        self.diagnose(doc.uri).await;
    }

    /// The change lands before the first `await` so changes apply in
    /// arrival order. See the `buffer` module.
    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        let revision = self.buffers.change(
            &uri,
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
        if self.buffers.touch(&uri).is_some() {
            self.diagnose(uri).await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        self.buffers.close(&uri);
        self.documents.write().await.remove(&uri);
    }

    /// A change on disk to a file that is not open may change what an
    /// open file sees, so every open document is analyzed again.
    /// Open files are already covered by `didChange`.
    async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        let external = params
            .changes
            .iter()
            .any(|change| !self.buffers.is_open(&change.uri));
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
        let Some(buffer) = self.buffers.snapshot(&uri) else {
            return Ok(None);
        };
        let path = uri_to_path(&uri).unwrap_or_default();
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
