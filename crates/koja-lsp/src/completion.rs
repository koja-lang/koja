//! Completion provider for the Koja LSP.
//!
//! Offers keyword completions, symbol completions, and dot-completions
//! (methods and fields on a type) based on the type registry.

use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;

use koja_ast::ast::{ExprKind, Visibility};
use koja_ast::identifier::GlobalRegistryId;
use koja_typecheck::{GlobalKind, GlobalRegistry};

use crate::backend::Backend;
use crate::format::{format_function_signature, format_resolved_type};
use crate::lookup::{LookupCtx, find_expr_at, traverse_receiver_type_id};

/// Koja language keywords offered as completions.
const KEYWORDS: &[&str] = &[
    "break", "cond", "const", "else", "end", "enum", "extend", "false", "fn", "for", "if", "impl",
    "in", "loop", "match", "move", "priv", "protocol", "receive", "return", "self", "spawn",
    "struct", "true", "type", "unless", "when", "while",
];

impl Backend {
    /// Handles `textDocument/completion` requests by returning keyword
    /// completions and known symbols from the registry, filtered to the
    /// prefix at the cursor.
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
        let (file, registry) = match (state.active_file(), state.registry()) {
            (Some(f), Some(r)) => (f, r),
            _ => return Ok(Some(CompletionResponse::Array(items))),
        };

        let ctx = LookupCtx {
            registry,
            package: &state.active_package,
            locals: &state.locals,
        };

        let line = pos.line + 1;
        let col = pos.character + 1;
        if let Some(expr) = find_expr_at(file, line, col)
            && let ExprKind::FieldAccess { receiver, .. } = &expr.kind
            && let Some(type_id) = traverse_receiver_type_id(receiver, &ctx)
        {
            let is_static = matches!(&receiver.kind, ExprKind::Ident { .. })
                && matches!(
                    registry.get(type_id).map(|e| &e.kind),
                    Some(GlobalKind::Struct(_) | GlobalKind::Enum(_))
                )
                && receiver.resolution == koja_ast::identifier::ResolvedType::Unresolved;
            add_dot_completions(type_id, is_static, registry, &mut items);
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

        add_symbol_completions(registry, &state.active_package, &prefix_lower, &mut items);
        if state.active_package != "Global" {
            add_symbol_completions(registry, "Global", &prefix_lower, &mut items);
        }

        Ok(Some(CompletionResponse::Array(items)))
    }
}

/// Add completion items for a type's methods (and, for instance
/// dispatch, its fields). `type_id` identifies the receiver's type in
/// the type registry; `is_static` switches between static (`Type.x`)
/// and instance (`value.x`) dispatch.
fn add_dot_completions(
    type_id: GlobalRegistryId,
    is_static: bool,
    registry: &GlobalRegistry,
    items: &mut Vec<CompletionItem>,
) {
    let entry = match registry.get(type_id) {
        Some(e) => e,
        None => return,
    };
    let pkg = entry.identifier.package().to_string();
    let type_name = entry.identifier.last().to_string();

    for (_, m_entry) in registry.iter() {
        let path = m_entry.identifier.path();
        if path.len() != 2 || path[0] != type_name {
            continue;
        }
        if m_entry.identifier.package() != pkg {
            continue;
        }
        let GlobalKind::Function(Some(sig)) = &m_entry.kind else {
            continue;
        };
        let dispatch_matches = match sig.dispatch {
            koja_typecheck::Dispatch::Instance => !is_static,
            koja_typecheck::Dispatch::Static => is_static,
        };
        if !dispatch_matches {
            continue;
        }
        let detail =
            format_function_signature(path[1].as_str(), sig, &m_entry.type_params, registry);
        items.push(CompletionItem {
            label: path[1].clone(),
            kind: Some(CompletionItemKind::METHOD),
            detail: Some(detail),
            ..Default::default()
        });
    }

    if !is_static && let GlobalKind::Struct(Some(def)) = &entry.kind {
        for field in &def.fields {
            items.push(CompletionItem {
                label: field.name.clone(),
                kind: Some(CompletionItemKind::FIELD),
                detail: Some(format_resolved_type(&field.ty, registry)),
                ..Default::default()
            });
        }
    }
}

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

/// Append completion items for non-method registry entries in `pkg`
/// whose names match `prefix_lower`.
fn add_symbol_completions(
    registry: &GlobalRegistry,
    pkg: &str,
    prefix_lower: &str,
    items: &mut Vec<CompletionItem>,
) {
    let matches =
        |name: &str| prefix_lower.is_empty() || name.to_ascii_lowercase().starts_with(prefix_lower);

    for (_, entry) in registry.iter_in_package(pkg) {
        let path = entry.identifier.path();
        if path.len() != 1 {
            continue;
        }
        let name = &path[0];
        if !matches(name) {
            continue;
        }
        let (kind, detail) = match &entry.kind {
            GlobalKind::Function(Some(sig)) => {
                if sig.params.iter().any(|p| p.name == "self") {
                    continue;
                }
                if visibility_for(sig) == Visibility::Private {
                    continue;
                }
                (
                    CompletionItemKind::FUNCTION,
                    Some(format_function_signature(
                        name,
                        sig,
                        &entry.type_params,
                        registry,
                    )),
                )
            }
            GlobalKind::Function(None) => continue,
            GlobalKind::Struct(_) => (
                CompletionItemKind::STRUCT,
                type_params_detail(&entry.type_params),
            ),
            GlobalKind::Enum(_) => (
                CompletionItemKind::ENUM,
                type_params_detail(&entry.type_params),
            ),
            GlobalKind::Protocol(_) => (
                CompletionItemKind::INTERFACE,
                type_params_detail(&entry.type_params),
            ),
            GlobalKind::Constant(Some(def)) => (
                CompletionItemKind::CONSTANT,
                Some(format_resolved_type(&def.ty, registry)),
            ),
            GlobalKind::Constant(None) => (CompletionItemKind::CONSTANT, None),
            GlobalKind::TypeAlias(Some(t)) => (
                CompletionItemKind::TYPE_PARAMETER,
                Some(format_resolved_type(t, registry)),
            ),
            GlobalKind::TypeAlias(None) => (CompletionItemKind::TYPE_PARAMETER, None),
        };
        items.push(CompletionItem {
            label: name.clone(),
            kind: Some(kind),
            detail,
            ..Default::default()
        });
    }
}

fn type_params_detail(params: &[String]) -> Option<String> {
    if params.is_empty() {
        None
    } else {
        Some(format!("<{}>", params.join(", ")))
    }
}

/// Today's [`FunctionSignature`] doesn't carry visibility; we
/// always treat it as public. Kept as a tiny helper so adding it later
/// is a one-line change.
fn visibility_for(_sig: &koja_typecheck::FunctionSignature) -> Visibility {
    Visibility::Public
}
