//! Completion provider for the Expo LSP.
//!
//! Offers keyword completions and symbol completions (functions, structs,
//! enums, constants, imported modules) based on the type-checking context
//! of the current document.

use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;

use expo_ast::ast::Visibility;

use crate::backend::Backend;

/// Expo language keywords offered as completions.
const KEYWORDS: &[&str] = &[
    "arena", "break", "cond", "const", "else", "end", "enum", "false", "fn", "for", "if", "impl",
    "import", "in", "loop", "match", "move", "priv", "protocol", "receive", "return", "self",
    "shared", "spawn", "struct", "true", "type", "unless", "when", "while",
];

impl Backend {
    /// Handles `textDocument/completion` requests by returning keyword
    /// completions and known symbols from the current type context.
    pub(crate) async fn handle_completion(
        &self,
        params: CompletionParams,
    ) -> Result<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri;
        let mut items = Vec::new();

        for kw in KEYWORDS {
            items.push(CompletionItem {
                label: kw.to_string(),
                kind: Some(CompletionItemKind::KEYWORD),
                ..Default::default()
            });
        }

        let docs = self.documents.read().await;
        if let Some(state) = docs.get(uri.as_str()) {
            add_symbol_completions(&state.ctx, &mut items);
            add_symbol_completions(&self.stdlib_ctx, &mut items);
        }

        Ok(Some(CompletionResponse::Array(items)))
    }
}

/// Appends completion items for all known symbols in a type context.
fn add_symbol_completions(
    ctx: &expo_typecheck::context::TypeContext,
    items: &mut Vec<CompletionItem>,
) {
    for (name, sig) in &ctx.functions {
        if sig.visibility == Visibility::Private {
            continue;
        }
        let params_str: Vec<String> = sig
            .params
            .iter()
            .map(|p| format!("{}: {}", p.name, p.ty.display()))
            .collect();
        let detail = format!(
            "fn({}) -> {}",
            params_str.join(", "),
            sig.return_type.display()
        );
        items.push(CompletionItem {
            label: name.clone(),
            kind: Some(CompletionItemKind::FUNCTION),
            detail: Some(detail),
            ..Default::default()
        });
    }

    for (name, info) in &ctx.structs {
        let detail = if info.type_params.is_empty() {
            None
        } else {
            Some(format!("<{}>", info.type_params.join(", ")))
        };
        items.push(CompletionItem {
            label: name.clone(),
            kind: Some(CompletionItemKind::STRUCT),
            detail,
            ..Default::default()
        });
    }

    for (name, info) in &ctx.enums {
        let detail = if info.type_params.is_empty() {
            None
        } else {
            Some(format!("<{}>", info.type_params.join(", ")))
        };
        items.push(CompletionItem {
            label: name.clone(),
            kind: Some(CompletionItemKind::ENUM),
            detail,
            ..Default::default()
        });
    }

    for (name, ty) in &ctx.constants {
        items.push(CompletionItem {
            label: name.clone(),
            kind: Some(CompletionItemKind::CONSTANT),
            detail: Some(ty.display()),
            ..Default::default()
        });
    }

    for name in ctx.imported_modules.keys() {
        items.push(CompletionItem {
            label: name.clone(),
            kind: Some(CompletionItemKind::MODULE),
            ..Default::default()
        });
    }
}
