//! Core LSP backend and server state.
//!
//! Defines the [`Backend`] server, document state management, and the
//! [`LanguageServer`] trait implementation that dispatches to focused
//! handler modules.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;
use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;
use tower_lsp_server::{Client, LanguageServer};

use expo_ast::ast::Module;
use expo_typecheck::context::TypeContext;

/// Cached state for a single open document, including the parsed AST,
/// type-checking context, and import origin mappings.
pub(crate) struct DocumentState {
    pub(crate) module: Module,
    pub(crate) ctx: TypeContext,
    #[allow(dead_code)]
    pub(crate) source: String,
    pub(crate) imported_origins: HashMap<String, String>,
    pub(crate) module_uris: HashMap<String, String>,
}

/// The Expo language server backend.
///
/// Holds shared state (stdlib context, open documents) and the LSP client
/// handle used to push diagnostics and notifications.
pub struct Backend {
    pub(crate) client: Client,
    pub(crate) documents: Arc<RwLock<HashMap<String, DocumentState>>>,
    pub(crate) stdlib_ctx: TypeContext,
    pub(crate) stdlib_modules: Vec<Module>,
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
    /// Creates a new backend, pre-loading all stdlib modules.
    pub fn new(client: Client) -> Self {
        let mut ctx = expo_typecheck::context::TypeContext::new();
        let mut stdlib_modules = Vec::new();

        for source in expo_typecheck::STDLIB_SOURCES {
            let parsed = expo_parser::parse(source);
            let mut mod_ctx = expo_typecheck::collect_module(&parsed.module);
            expo_typecheck::merge_stdlib(&ctx, &mut mod_ctx);
            expo_typecheck::merge_stdlib(&mod_ctx, &mut ctx);
            stdlib_modules.push(parsed.module);
        }

        Self {
            client,
            documents: Arc::new(RwLock::new(HashMap::new())),
            stdlib_ctx: ctx,
            stdlib_modules,
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
                ..Default::default()
            },
            server_info: Some(ServerInfo {
                name: "expo-lsp".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "expo-lsp initialized")
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

    async fn formatting(&self, params: DocumentFormattingParams) -> Result<Option<Vec<TextEdit>>> {
        let uri = params.text_document.uri;

        let uri_str = uri.as_str();
        let path = uri_str
            .strip_prefix("file://")
            .ok_or_else(|| tower_lsp_server::jsonrpc::Error::invalid_params("invalid file URI"))?;

        let source = std::fs::read_to_string(path)
            .map_err(|_| tower_lsp_server::jsonrpc::Error::invalid_params("could not read file"))?;

        match expo_fmt::format(&source) {
            expo_fmt::FormatResult::Ok(formatted) => {
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
            expo_fmt::FormatResult::ParseErrors(_) => Ok(None),
        }
    }
}
