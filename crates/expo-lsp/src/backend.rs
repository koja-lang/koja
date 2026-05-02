//! Core LSP backend and server state.
//!
//! Defines the [`Backend`] server, document state management, and the
//! [`LanguageServer`] trait implementation that dispatches to focused
//! handler modules.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use tokio::sync::RwLock;
use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;
use tower_lsp_server::{Client, LanguageServer};

use expo_ast::ast::Module;
use expo_typecheck::context::TypeContext;
use expo_typecheck::types::{Package, fqn_to_package};

/// Cached state for a single open document, including the parsed AST
/// and type-checking context.
pub(crate) struct DocumentState {
    pub(crate) module: Module,
    pub(crate) ctx: TypeContext,
    pub(crate) source: String,
    pub(crate) project_modules: Vec<Module>,
}

/// The Expo language server backend.
///
/// Holds shared state (stdlib context, open documents) and the LSP client
/// handle used to push diagnostics and notifications.
pub struct Backend {
    pub(crate) client: Client,
    pub(crate) documents: Arc<RwLock<HashMap<String, DocumentState>>>,
    pub(crate) project_modules: Arc<RwLock<Vec<Module>>>,
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
        let mut source_names = Vec::new();

        for &(name, source) in expo_stdlib::SOURCES {
            let parsed = expo_parser::parse(source);
            source_names.push(name);
            stdlib_modules.push(parsed.module);
        }

        let stdlib_refs: Vec<&Module> = stdlib_modules.iter().collect();
        let mut known_packages: BTreeSet<Package> = BTreeSet::from([Package::Std]);
        for name in &source_names {
            if !name.starts_with("std.") {
                known_packages.insert(Package::Named(fqn_to_package(name).to_string()));
            }
        }
        let global_names = expo_typecheck::collect_all_names(&stdlib_refs, known_packages);

        // Collect all stdlib modules. Auto-imported modules (std.*) use
        // package "std". Qualified modules (json, net, etc.) use their
        // package name as the identifier, making them accessible via
        // ctx.is_package_type() for alias resolution.
        // `collect_file` is `&mut` because it runs the synthesize
        // sub-pass internally (auto-derives `impl Debug`).
        for (i, module) in stdlib_modules.iter_mut().enumerate() {
            let name = source_names[i];
            let pkg = if name.starts_with("std.") {
                "std"
            } else {
                fqn_to_package(name)
            };
            let mut mod_ctx = expo_typecheck::collect_file(module, &global_names, pkg);
            mod_ctx.merge(&ctx);
            ctx.merge(&mod_ctx);
        }

        expo_typecheck::resolve_packages(&mut ctx);

        Self {
            client,
            documents: Arc::new(RwLock::new(HashMap::new())),
            project_modules: Arc::new(RwLock::new(Vec::new())),
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
                workspace_symbol_provider: Some(OneOf::Left(true)),
                folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
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

        match expo_fmt::format(source) {
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
