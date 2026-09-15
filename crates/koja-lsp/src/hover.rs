//! Hover information provider for the Koja LSP.
//!
//! Renders the signature and `@doc` text of the symbol under the
//! cursor. The symbol comes from the reference index, and the
//! signature comes from its registry entry. Function signatures are
//! laid out by `koja-fmt`, so they match what `koja format` writes.

use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;

use koja_ast::identifier::GlobalRegistryId;
use koja_query::display::{
    format_enum_def, format_protocol_def, format_resolved_type, format_struct_def,
};
use koja_query::symbol::SymbolKind;
use koja_query::{Analysis, SymbolKey, docs, signature};
use koja_typecheck::{GlobalKind, RegistryEntry};

use crate::backend::Backend;

impl Backend {
    /// Handles `textDocument/hover` requests by looking up the symbol under
    /// the cursor and building a Markdown hover response.
    pub(crate) async fn handle_hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        let docs = self.documents.read().await;
        let Some(state) = docs.get(uri.as_str()) else {
            return Ok(None);
        };
        let Some(analysis) = state.analysis() else {
            return Ok(None);
        };
        let Some(symbol) = state.symbol_at(&analysis, position) else {
            return Ok(None);
        };

        let hover_text = match &symbol.kind {
            SymbolKind::Global(entry) => {
                let SymbolKey::Global(id) = symbol.key else {
                    return Ok(None);
                };
                global_hover(&analysis, id, entry)
            }
            SymbolKind::Local { ty } => {
                let signature = match ty {
                    Some(ty) => format!(
                        "{}: {}",
                        symbol.name,
                        format_resolved_type(ty, analysis.registry)
                    ),
                    None => symbol.name.clone(),
                };
                Some(format_hover(&signature, None))
            }
            SymbolKind::TypeParam => Some(format_hover(
                &format!("{} (type parameter)", symbol.name),
                None,
            )),
        };

        Ok(hover_text.map(|text| Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: text,
            }),
            range: None,
        }))
    }
}

/// Signature plus `@doc` for a registry entry. `None` for an entry
/// whose definition is not lifted yet.
fn global_hover(
    analysis: &Analysis<'_>,
    id: GlobalRegistryId,
    entry: &RegistryEntry,
) -> Option<String> {
    let registry = analysis.registry;
    let name = entry.identifier.last();
    let type_params = entry.type_params.as_slice();
    let signature = match &entry.kind {
        GlobalKind::Function(_) => signature::function_signature(analysis, id)?,
        GlobalKind::Builtin(_) => format!("builtin {name}{}", type_params_display(type_params)),
        GlobalKind::Struct(Some(def)) => {
            format_struct_def(name, type_params, &def.fields, registry)
        }
        GlobalKind::Enum(Some(def)) => format_enum_def(name, type_params, &def.variants, registry),
        GlobalKind::Protocol(Some(def)) => {
            // The registry stores the implicit `Self` at index 0.
            let declared = type_params
                .strip_prefix(&["Self".to_string()][..])
                .unwrap_or(type_params);
            format_protocol_def(name, declared, &def.methods, registry)
        }
        GlobalKind::Constant(Some(def)) => {
            format!("const {name}: {}", format_resolved_type(&def.ty, registry))
        }
        GlobalKind::TypeAlias(Some(expansion)) => {
            format!(
                "type {name} = {}",
                format_resolved_type(expansion, registry)
            )
        }
        GlobalKind::Struct(None)
        | GlobalKind::Enum(None)
        | GlobalKind::Protocol(None)
        | GlobalKind::Constant(None)
        | GlobalKind::TypeAlias(None) => return None,
    };
    let doc = docs::doc_for(analysis, id);
    Some(format_hover(&signature, doc.as_deref()))
}

fn type_params_display(type_params: &[String]) -> String {
    if type_params.is_empty() {
        String::new()
    } else {
        format!("<{}>", type_params.join(", "))
    }
}

/// Render the hover body as a Markdown code block with optional
/// documentation appended below a separator.
fn format_hover(signature: &str, doc: Option<&str>) -> String {
    let mut md = format!("```koja\n{signature}\n```");
    if let Some(d) = doc {
        md.push_str("\n\n---\n\n");
        md.push_str(d);
    }
    md
}
