use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::RwLock;
use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;
use tower_lsp_server::{Client, LanguageServer};

use expo_ast::ast::{
    AnnotationValue, Diagnostic as ExpoDiagnostic, ImportTarget, Item, Module,
    Severity as ExpoSeverity,
};
use expo_typecheck::context::{TypeContext, VariantData};

use crate::lookup::{self, SymbolInfo};

struct DocumentState {
    module: Module,
    ctx: TypeContext,
    #[allow(dead_code)]
    source: String,
    imported_origins: HashMap<String, String>,
    module_uris: HashMap<String, String>,
}

#[derive(Debug)]
pub struct Backend {
    client: Client,
    documents: Arc<RwLock<HashMap<String, DocumentState>>>,
}

impl std::fmt::Debug for DocumentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DocumentState").finish()
    }
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            documents: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    async fn diagnose(&self, uri: Uri, text: &str, version: Option<i32>) {
        let parse_result = expo_parser::parse(text);

        let mut all_diags: Vec<ExpoDiagnostic> = parse_result.errors;

        let (ctx, imported_origins, module_uris) = if all_diags
            .iter()
            .all(|d| !matches!(d.severity, ExpoSeverity::Error))
        {
            let mut module_contexts: HashMap<String, TypeContext> = HashMap::new();
            let mut origins: HashMap<String, String> = HashMap::new();
            let mut mod_uris: HashMap<String, String> = HashMap::new();

            for item in &parse_result.module.items {
                if let Item::Import(import) = item {
                    let (resolve_path, module_key, qualifier) = match &import.target {
                        ImportTarget::Item(name) => {
                            let mut full = import.path.clone();
                            full.push(name.clone());
                            let key = full.join(".");
                            (full, key, Some(name.clone()))
                        }
                        _ => {
                            let key = import.path.join(".");
                            let q = import.path.last().cloned();
                            (import.path.clone(), key, q)
                        }
                    };

                    if let Some(dep_path) = resolve_module_file(&uri, &resolve_path)
                        && let Ok(dep_source) = std::fs::read_to_string(&dep_path)
                    {
                        let dep_parsed = expo_parser::parse(&dep_source);
                        let dep_ctx = expo_typecheck::collect_module(&dep_parsed.module);
                        let dep_uri = format!("file://{}", dep_path.display());

                        for (name, sig) in &dep_ctx.functions {
                            if !sig.is_private {
                                origins.insert(name.clone(), dep_uri.clone());
                            }
                        }
                        for name in dep_ctx.structs.keys() {
                            origins.insert(name.clone(), dep_uri.clone());
                        }
                        for name in dep_ctx.enums.keys() {
                            origins.insert(name.clone(), dep_uri.clone());
                        }

                        if let Some(q) = qualifier {
                            mod_uris.insert(q, dep_uri);
                        }

                        module_contexts.insert(module_key, dep_ctx);
                    }
                }
            }

            let mut ctx = expo_typecheck::collect_module(&parse_result.module);
            expo_typecheck::resolve_imports(&parse_result.module, &mut ctx, &module_contexts);
            expo_typecheck::check_module(&parse_result.module, &mut ctx);
            all_diags.extend(ctx.diagnostics.clone());
            (ctx, origins, mod_uris)
        } else {
            (TypeContext::new(), HashMap::new(), HashMap::new())
        };

        {
            let mut docs = self.documents.write().await;
            docs.insert(
                uri.as_str().to_string(),
                DocumentState {
                    module: parse_result.module,
                    ctx,
                    source: text.to_string(),
                    imported_origins,
                    module_uris,
                },
            );
        }

        let lsp_diags: Vec<Diagnostic> = all_diags.iter().map(to_lsp_diagnostic).collect();

        for d in &lsp_diags {
            eprintln!(
                "[expo-lsp] diag: {:?} L{}:{}-L{}:{} \"{}\"",
                d.severity,
                d.range.start.line,
                d.range.start.character,
                d.range.end.line,
                d.range.end.character,
                d.message,
            );
        }

        self.client
            .publish_diagnostics(uri, lsp_diags, version)
            .await;
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
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        let line = pos.line + 1;
        let col = pos.character + 1;

        let docs = self.documents.read().await;
        let state = match docs.get(uri.as_str()) {
            Some(s) => s,
            None => return Ok(None),
        };

        let symbol = match lookup::find_symbol_at(&state.module, line, col, &state.ctx) {
            Some(s) => s,
            None => return Ok(None),
        };

        let hover_text = match &symbol {
            SymbolInfo::Function { name } => {
                if let Some(sig) = state.ctx.functions.get(name) {
                    let params_str: Vec<String> = sig
                        .params
                        .iter()
                        .map(|p| format!("{}: {}", p.name, p.ty.display()))
                        .collect();
                    let vis = if sig.is_private { "priv fn" } else { "fn" };
                    let tp = if sig.type_params.is_empty() {
                        String::new()
                    } else {
                        format!("<{}>", sig.type_params.join(", "))
                    };
                    let signature = format!(
                        "{} {}{}({}) -> {}",
                        vis,
                        name,
                        tp,
                        params_str.join(", "),
                        sig.return_type.display()
                    );
                    let doc = if let Some(origin_uri) = state.imported_origins.get(name) {
                        find_doc_from_uri(origin_uri, name)
                    } else {
                        lookup::find_doc_for(&state.module, name)
                    };
                    format_hover(&signature, doc.as_deref())
                } else {
                    return Ok(None);
                }
            }
            SymbolInfo::Struct { name } => {
                if let Some(info) = state.ctx.structs.get(name) {
                    let fields: Vec<String> = info
                        .fields
                        .iter()
                        .map(|(n, t)| format!("  {}: {}", n, t.display()))
                        .collect();
                    let tp = if info.type_params.is_empty() {
                        String::new()
                    } else {
                        format!("<{}>", info.type_params.join(", "))
                    };
                    let signature = format!("struct {}{}\n{}\nend", name, tp, fields.join("\n"));
                    let doc = if let Some(origin_uri) = state.imported_origins.get(name) {
                        find_doc_from_uri(origin_uri, name)
                    } else {
                        lookup::find_doc_for(&state.module, name)
                    };
                    format_hover(&signature, doc.as_deref())
                } else {
                    return Ok(None);
                }
            }
            SymbolInfo::Constant { name } => {
                if let Some(ty) = state.ctx.constants.get(name) {
                    let signature = format!("const {}: {}", name, ty.display());
                    let doc = lookup::find_doc_for(&state.module, name);
                    format_hover(&signature, doc.as_deref())
                } else {
                    return Ok(None);
                }
            }
            SymbolInfo::Enum { name } => {
                if let Some(info) = state.ctx.enums.get(name) {
                    let variants: Vec<String> = info
                        .variants
                        .iter()
                        .map(|v| match &v.data {
                            VariantData::Unit => format!("  {}", v.name),
                            VariantData::Tuple(types) => {
                                let ts: Vec<String> = types.iter().map(|t| t.display()).collect();
                                format!("  {}({})", v.name, ts.join(", "))
                            }
                            VariantData::Struct(fields) => {
                                let fs: Vec<String> = fields
                                    .iter()
                                    .map(|(n, t)| format!("{}: {}", n, t.display()))
                                    .collect();
                                format!("  {}{{{}}}", v.name, fs.join(", "))
                            }
                        })
                        .collect();
                    let signature = format!("enum {}\n{}\nend", name, variants.join("\n"));
                    let doc = if let Some(origin_uri) = state.imported_origins.get(name) {
                        find_doc_from_uri(origin_uri, name)
                    } else {
                        lookup::find_doc_for(&state.module, name)
                    };
                    format_hover(&signature, doc.as_deref())
                } else {
                    return Ok(None);
                }
            }
            SymbolInfo::ModuleFunction { module, name } => {
                let sig = state
                    .ctx
                    .imported_modules
                    .get(module)
                    .and_then(|m| m.functions.get(name));
                if let Some(sig) = sig {
                    let params_str: Vec<String> = sig
                        .params
                        .iter()
                        .map(|p| format!("{}: {}", p.name, p.ty.display()))
                        .collect();
                    let tp = if sig.type_params.is_empty() {
                        String::new()
                    } else {
                        format!("<{}>", sig.type_params.join(", "))
                    };
                    let signature = format!(
                        "fn {}.{}{}({}) -> {}",
                        module,
                        name,
                        tp,
                        params_str.join(", "),
                        sig.return_type.display()
                    );
                    let doc = state
                        .module_uris
                        .get(module)
                        .and_then(|uri_str| find_doc_from_uri(uri_str, name));
                    format_hover(&signature, doc.as_deref())
                } else {
                    return Ok(None);
                }
            }
            SymbolInfo::Variable { name } => format!("```expo\n{}\n```", name),
            SymbolInfo::Module { path } => {
                let module_name = path.join(".");
                let signature = format!("module {}", module_name);
                let doc = state
                    .module_uris
                    .get(&module_name)
                    .and_then(|uri_str| {
                        let p = uri_str.strip_prefix("file://")?;
                        std::fs::read_to_string(p).ok()
                    })
                    .or_else(|| {
                        resolve_module_file(&uri, path)
                            .and_then(|p| std::fs::read_to_string(p).ok())
                    })
                    .and_then(|source| {
                        let parsed = expo_parser::parse(&source);
                        parsed.module.moduledoc.and_then(|d| match d.value {
                            Some(AnnotationValue::String(s)) => Some(s),
                            _ => None,
                        })
                    });
                format_hover(&signature, doc.as_deref())
            }
        };

        Ok(Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: hover_text,
            }),
            range: None,
        }))
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        let line = pos.line + 1;
        let col = pos.character + 1;

        let docs = self.documents.read().await;
        let state = match docs.get(uri.as_str()) {
            Some(s) => s,
            None => return Ok(None),
        };

        let symbol = match lookup::find_symbol_at(&state.module, line, col, &state.ctx) {
            Some(s) => s,
            None => return Ok(None),
        };

        if let SymbolInfo::Module { ref path } = symbol {
            let module_name = path.join(".");
            let target_uri_str = state.module_uris.get(&module_name).cloned().or_else(|| {
                resolve_module_file(&uri, path).map(|p| format!("file://{}", p.display()))
            });
            if let Some(uri_str) = target_uri_str {
                let target_uri: Uri = uri_str.parse().map_err(|_| {
                    tower_lsp_server::jsonrpc::Error::invalid_params("invalid module URI")
                })?;
                return Ok(Some(GotoDefinitionResponse::Scalar(Location {
                    uri: target_uri,
                    range: Range {
                        start: Position::new(0, 0),
                        end: Position::new(0, 0),
                    },
                })));
            }
            return Ok(None);
        }

        if let SymbolInfo::ModuleFunction {
            ref module,
            ref name,
        } = symbol
        {
            if let Some(module_uri) = state.module_uris.get(module) {
                return goto_definition_in_file(module_uri, name);
            }
            return Ok(None);
        }

        let symbol_name = match &symbol {
            SymbolInfo::Function { name }
            | SymbolInfo::Struct { name }
            | SymbolInfo::Enum { name }
            | SymbolInfo::Constant { name } => Some(name.as_str()),
            _ => None,
        };

        if let Some(name) = symbol_name
            && let Some(origin_uri_str) = state.imported_origins.get(name)
        {
            return goto_definition_in_file(origin_uri_str, name);
        }

        let def_span = match &symbol {
            SymbolInfo::Function { name } => state.ctx.functions.get(name).map(|sig| sig.span),
            SymbolInfo::Struct { name } => state.ctx.structs.get(name).map(|info| info.span),
            SymbolInfo::Enum { name } => state.ctx.enums.get(name).map(|info| info.span),
            SymbolInfo::Constant { name } => state.module.items.iter().find_map(|item| {
                if let Item::Constant(c) = item
                    && c.name == *name
                {
                    Some(c.span)
                } else {
                    None
                }
            }),
            SymbolInfo::Variable { .. }
            | SymbolInfo::Module { .. }
            | SymbolInfo::ModuleFunction { .. } => None,
        };

        let span = match def_span {
            Some(s) => s,
            None => return Ok(None),
        };

        let range = Range {
            start: Position::new(
                span.start.line.saturating_sub(1),
                span.start.column.saturating_sub(1),
            ),
            end: Position::new(
                span.end.line.saturating_sub(1),
                span.end.column.saturating_sub(1),
            ),
        };

        Ok(Some(GotoDefinitionResponse::Scalar(Location {
            uri: uri.clone(),
            range,
        })))
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

fn resolve_module_file(current_uri: &Uri, module_path: &[String]) -> Option<PathBuf> {
    let uri_str = current_uri.as_str();
    let file_path = uri_str.strip_prefix("file://")?;
    let current = PathBuf::from(file_path);
    let dir = current.parent()?;

    let mut candidate = dir.to_path_buf();
    for segment in module_path {
        candidate.push(segment);
    }
    candidate.set_extension("expo");

    if candidate.exists() {
        Some(candidate)
    } else {
        None
    }
}

fn find_doc_from_uri(uri_str: &str, name: &str) -> Option<String> {
    let path = uri_str.strip_prefix("file://")?;
    let source = std::fs::read_to_string(path).ok()?;
    let parsed = expo_parser::parse(&source);
    lookup::find_doc_for(&parsed.module, name)
}

fn goto_definition_in_file(uri_str: &str, name: &str) -> Result<Option<GotoDefinitionResponse>> {
    let path = match uri_str.strip_prefix("file://") {
        Some(p) => p,
        None => return Ok(None),
    };
    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return Ok(None),
    };
    let parsed = expo_parser::parse(&source);
    let ctx = expo_typecheck::collect_module(&parsed.module);

    let span = if let Some(sig) = ctx.functions.get(name) {
        Some(sig.span)
    } else if let Some(info) = ctx.structs.get(name) {
        Some(info.span)
    } else {
        ctx.enums.get(name).map(|info| info.span)
    };

    let span = match span {
        Some(s) => s,
        None => return Ok(None),
    };

    let target_uri: Uri = uri_str
        .parse()
        .map_err(|_| tower_lsp_server::jsonrpc::Error::invalid_params("invalid URI"))?;

    let range = Range {
        start: Position::new(
            span.start.line.saturating_sub(1),
            span.start.column.saturating_sub(1),
        ),
        end: Position::new(
            span.end.line.saturating_sub(1),
            span.end.column.saturating_sub(1),
        ),
    };

    Ok(Some(GotoDefinitionResponse::Scalar(Location {
        uri: target_uri,
        range,
    })))
}

fn format_hover(signature: &str, doc: Option<&str>) -> String {
    let mut md = format!("```expo\n{}\n```", signature);
    if let Some(d) = doc {
        md.push_str("\n\n---\n\n");
        md.push_str(d);
    }
    md
}

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
        range: Range {
            start: Position::new(
                d.span.start.line.saturating_sub(1),
                d.span.start.column.saturating_sub(1),
            ),
            end: Position::new(
                d.span.end.line.saturating_sub(1),
                d.span.end.column.saturating_sub(1),
            ),
        },
        severity: Some(severity),
        source: Some("expo".to_string()),
        message,
        ..Default::default()
    }
}
