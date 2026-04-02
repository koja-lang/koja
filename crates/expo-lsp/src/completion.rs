//! Completion provider for the Expo LSP.
//!
//! Offers keyword completions, symbol completions, and dot-completions
//! (methods and fields on a type) based on the type-checking context.

use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;

use expo_ast::ast::Visibility;
use expo_typecheck::context::{FunctionKind, TypeContext};

use crate::backend::Backend;
use crate::lookup::receiver::resolve_receiver_type;

/// Expo language keywords offered as completions.
const KEYWORDS: &[&str] = &[
    "arena", "break", "cond", "const", "else", "end", "enum", "false", "fn", "for", "if", "impl",
    "in", "loop", "match", "move", "priv", "protocol", "receive", "return", "self", "shared",
    "spawn", "struct", "true", "type", "unless", "when", "while",
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
        let state = match docs.get(uri.as_str()) {
            Some(s) => s,
            None => return Ok(Some(CompletionResponse::Array(items))),
        };

        if let Some(receiver) = detect_dot_context(&state.source, pos) {
            let is_uppercase = receiver.chars().next().is_some_and(|c| c.is_uppercase());
            if let Some(type_name) = resolve_receiver_type(&receiver, &state.source, &state.ctx) {
                add_dot_completions(&type_name, is_uppercase, &state.ctx, &mut items);
                add_dot_completions(&type_name, is_uppercase, &self.stdlib_ctx, &mut items);
            }
            return Ok(Some(CompletionResponse::Array(items)));
        }

        let prefix = word_prefix_at(&state.source, pos);
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

        add_symbol_completions(&state.ctx, &prefix_lower, &mut items);
        add_symbol_completions(&self.stdlib_ctx, &prefix_lower, &mut items);

        Ok(Some(CompletionResponse::Array(items)))
    }
}

/// Checks if the cursor is immediately after a `.` and returns the receiver
/// token (the identifier before the dot).
fn detect_dot_context(source: &str, pos: Position) -> Option<String> {
    let lines: Vec<&str> = source.lines().collect();
    let line_idx = pos.line as usize;
    if line_idx >= lines.len() {
        return None;
    }
    let line = lines[line_idx];
    let col = (pos.character as usize).min(line.len());
    let before = &line[..col];

    // Strip any partial identifier the user has typed after the dot
    let before = before.trim_end_matches(|c: char| c.is_alphanumeric() || c == '_');

    // Must end with a dot
    let before = before.strip_suffix('.')?;

    // Extract the receiver token before the dot
    let receiver: String = before
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    if receiver.is_empty() {
        return None;
    }

    Some(receiver)
}

/// Adds completion items for methods and fields available on a type.
fn add_dot_completions(
    type_name: &str,
    is_static: bool,
    ctx: &TypeContext,
    items: &mut Vec<CompletionItem>,
) {
    let info = match ctx.types.get(type_name) {
        Some(i) => i,
        None => return,
    };

    for (name, sig) in &info.functions {
        let matches_context = if is_static {
            sig.kind == FunctionKind::Static
        } else {
            matches!(sig.kind, FunctionKind::Instance(_))
        };
        if !matches_context || sig.visibility == Visibility::Private {
            continue;
        }

        let params_str: Vec<String> = sig
            .params
            .iter()
            .filter(|p| p.name != "self")
            .map(|p| format!("{}: {}", p.name, p.ty.display()))
            .collect();
        let detail = format!(
            "fn({}) -> {}",
            params_str.join(", "),
            sig.return_type.display()
        );
        items.push(CompletionItem {
            label: name.clone(),
            kind: Some(CompletionItemKind::METHOD),
            detail: Some(detail),
            ..Default::default()
        });
    }

    if !is_static && let Some(fields) = info.fields() {
        for (name, ty) in fields {
            items.push(CompletionItem {
                label: name.clone(),
                kind: Some(CompletionItemKind::FIELD),
                detail: Some(ty.display()),
                ..Default::default()
            });
        }
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
fn add_symbol_completions(ctx: &TypeContext, prefix_lower: &str, items: &mut Vec<CompletionItem>) {
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
            Some(format!(
                "<{}>",
                info.type_params
                    .iter()
                    .map(|p| p.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
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
            Some(format!(
                "<{}>",
                info.type_params
                    .iter()
                    .map(|p| p.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
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
}
