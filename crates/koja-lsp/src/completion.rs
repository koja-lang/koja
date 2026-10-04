//! Completion provider for the Koja LSP.
//!
//! Offers keyword completions, symbol completions, and dot-completions
//! (methods and fields on a type). Candidate enumeration lives on
//! [`koja_typecheck::GlobalRegistry`], shared with the REPL. This
//! module maps candidates onto LSP `CompletionItem`s.

use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;

use koja_ast::ast::ExprKind;
use koja_query::display::{format_function_signature, format_resolved_type};
use koja_query::expr_at::{find_expr_at, receiver_type_id};
use koja_typecheck::{Candidate, CandidateDetail, CandidateKind, GlobalKind, GlobalRegistry};

use crate::backend::Backend;
use crate::convert::Positions;

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

        self.with_analysis(&uri, |doc| {
            let Some(file) = doc.active_file() else {
                return Ok(None);
            };
            let registry = doc.analysis.registry;
            let mut items = Vec::new();

            let (line, col) = doc.line_column(pos);
            if let Some(expr) = find_expr_at(file, line, col)
                && let ExprKind::FieldAccess { receiver, .. } = &expr.kind
                && let Some(type_id) = receiver_type_id(receiver, registry)
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

            // The prefix comes from the live buffer, which can be a
            // few keystrokes ahead of the analyzed text.
            let prefix = match self.buffers.snapshot(&uri) {
                Some(buffer) => word_prefix_at(&Positions::new(self.encoding(), &buffer.text), pos),
                None => word_prefix_at(&doc.positions, pos),
            };
            let prefix_lower = prefix.to_ascii_lowercase();
            let matches = |name: &str| {
                prefix.is_empty() || name.to_ascii_lowercase().starts_with(&prefix_lower)
            };

            for kw in koja_typecheck::KEYWORDS {
                if matches(kw) {
                    items.push(CompletionItem {
                        label: kw.to_string(),
                        kind: Some(CompletionItemKind::KEYWORD),
                        ..Default::default()
                    });
                }
            }

            let active_package = doc.state.active_package.as_str();
            let mut packages = vec![active_package];
            if active_package != "Global" {
                packages.push("Global");
            }
            for pkg in packages {
                for candidate in registry.symbol_candidates(pkg, active_package) {
                    if matches(candidate.label) {
                        items.push(to_completion_item(&candidate, registry));
                    }
                }
            }
            Ok(Some(CompletionResponse::Array(items)))
        })
        .await
    }
}

/// Map a registry [`Candidate`] onto a `CompletionItem`, rendering
/// its detail through the LSP's signature / type formatters.
fn to_completion_item(candidate: &Candidate<'_>, registry: &GlobalRegistry) -> CompletionItem {
    let kind = match candidate.kind {
        CandidateKind::Builtin => CompletionItemKind::STRUCT,
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

fn word_prefix_at(positions: &Positions<'_>, pos: Position) -> String {
    let before = &positions.text()[..positions.offset(pos)];
    before
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}
