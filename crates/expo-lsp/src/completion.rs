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
    /// completions and known symbols from the current type context,
    /// filtered to the prefix at the cursor position.
    pub(crate) async fn handle_completion(
        &self,
        params: CompletionParams,
    ) -> Result<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let mut items = Vec::new();

        let docs = self.documents.read().await;
        let prefix = docs
            .get(uri.as_str())
            .map(|state| word_prefix_at(&state.source, pos))
            .unwrap_or_default();
        let prefix_lower = prefix.to_ascii_lowercase();

        for kw in KEYWORDS {
            if prefix.is_empty() || kw.starts_with(&prefix_lower) {
                items.push(CompletionItem {
                    label: kw.to_string(),
                    kind: Some(CompletionItemKind::KEYWORD),
                    ..Default::default()
                });
            }
        }

        if let Some(state) = docs.get(uri.as_str()) {
            add_symbol_completions(&state.ctx, &prefix_lower, &mut items);
            add_symbol_completions(&self.stdlib_ctx, &prefix_lower, &mut items);
        }

        Ok(Some(CompletionResponse::Array(items)))
    }
}

/// Extracts the partial identifier immediately before the cursor position.
fn word_prefix_at(source: &str, pos: Position) -> String {
    let lines: Vec<&str> = source.lines().collect();
    let line_idx = pos.line as usize;
    if line_idx >= lines.len() {
        return String::new();
    }
    let line = lines[line_idx];
    let col = (pos.character as usize).min(line.len());
    let before = &line[..col];
    before
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

/// Appends completion items for symbols in a type context whose names
/// match the given lowercase prefix.
fn add_symbol_completions(
    ctx: &expo_typecheck::context::TypeContext,
    prefix_lower: &str,
    items: &mut Vec<CompletionItem>,
) {
    let matches =
        |name: &str| prefix_lower.is_empty() || name.to_ascii_lowercase().starts_with(prefix_lower);

    for (name, sig) in &ctx.functions {
        if sig.visibility == Visibility::Private || !matches(name) {
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

    for (name, info) in ctx.types.iter().filter(|(_, ti)| ti.is_struct()) {
        if !matches(name) {
            continue;
        }
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

    for (name, info) in ctx.types.iter().filter(|(_, ti)| ti.is_enum()) {
        if !matches(name) {
            continue;
        }
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
        if !matches(name) {
            continue;
        }
        items.push(CompletionItem {
            label: name.clone(),
            kind: Some(CompletionItemKind::CONSTANT),
            detail: Some(ty.display()),
            ..Default::default()
        });
    }

    for name in ctx.imported_modules.keys() {
        if !matches(name) {
            continue;
        }
        items.push(CompletionItem {
            label: name.clone(),
            kind: Some(CompletionItemKind::MODULE),
            ..Default::default()
        });
    }
}
