//! Completion provider for the Koja LSP.
//!
//! Offers keyword completions, symbol completions, and dot-completions
//! (methods and fields on a type). Candidate enumeration lives on
//! [`koja_typecheck::GlobalRegistry`], shared with the REPL. This
//! module maps candidates onto LSP `CompletionItem`s.

use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;

use koja_ast::ast::ExprKind;
use koja_typecheck::{Candidate, CandidateDetail, CandidateKind, GlobalKind, GlobalRegistry};

use crate::backend::Backend;
use crate::format::{format_function_signature, format_resolved_type};
use crate::lookup::{LookupCtx, find_expr_at, traverse_receiver_type_id};

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
            for candidate in registry.dot_candidates(type_id, is_static) {
                items.push(to_completion_item(&candidate, registry));
            }
            return Ok(Some(CompletionResponse::Array(items)));
        }

        let prefix = word_prefix_at(&state.source, pos);
        let prefix_lower = prefix.to_ascii_lowercase();
        let matches =
            |name: &str| prefix.is_empty() || name.to_ascii_lowercase().starts_with(&prefix_lower);

        for kw in koja_typecheck::KEYWORDS {
            if matches(kw) {
                items.push(CompletionItem {
                    label: kw.to_string(),
                    kind: Some(CompletionItemKind::KEYWORD),
                    ..Default::default()
                });
            }
        }

        let mut packages = vec![state.active_package.as_str()];
        if state.active_package != "Global" {
            packages.push("Global");
        }
        for pkg in packages {
            for candidate in registry.symbol_candidates(pkg, &state.active_package) {
                if matches(candidate.label) {
                    items.push(to_completion_item(&candidate, registry));
                }
            }
        }

        Ok(Some(CompletionResponse::Array(items)))
    }
}

/// Map a registry [`Candidate`] onto a `CompletionItem`, rendering
/// its detail through the LSP's signature / type formatters.
fn to_completion_item(candidate: &Candidate<'_>, registry: &GlobalRegistry) -> CompletionItem {
    let kind = match candidate.kind {
        CandidateKind::Constant => CompletionItemKind::CONSTANT,
        CandidateKind::Enum => CompletionItemKind::ENUM,
        CandidateKind::EnumVariant => CompletionItemKind::ENUM_MEMBER,
        CandidateKind::Field => CompletionItemKind::FIELD,
        CandidateKind::Function => CompletionItemKind::FUNCTION,
        CandidateKind::Method => CompletionItemKind::METHOD,
        CandidateKind::Protocol => CompletionItemKind::INTERFACE,
        CandidateKind::Struct => CompletionItemKind::STRUCT,
        CandidateKind::TypeAlias => CompletionItemKind::TYPE_PARAMETER,
    };
    let detail = match candidate.detail {
        CandidateDetail::Function {
            signature,
            type_params,
        } => Some(format_function_signature(
            candidate.label,
            signature,
            type_params,
            registry,
        )),
        CandidateDetail::None => None,
        CandidateDetail::Type(ty) => Some(format_resolved_type(ty, registry)),
        CandidateDetail::TypeParams(params) => {
            (!params.is_empty()).then(|| format!("<{}>", params.join(", ")))
        }
    };
    CompletionItem {
        label: candidate.label.to_string(),
        kind: Some(kind),
        detail,
        ..Default::default()
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
