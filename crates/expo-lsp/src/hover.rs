//! Hover information provider for the Expo LSP.
//!
//! Builds rich hover content (signatures and doc comments) for functions,
//! structs, enums, constants, modules, and variables.

use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;

use expo_ast::ast::Module;
use expo_ast::identifier::TypeIdentifier;
use expo_typecheck::context::VariantData;

use crate::backend::{Backend, DocumentState};
use crate::lookup::{self, SymbolInfo};

impl Backend {
    /// Handles `textDocument/hover` requests by looking up the symbol under
    /// the cursor and building a Markdown hover response.
    pub(crate) async fn handle_hover(&self, params: HoverParams) -> Result<Option<Hover>> {
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
                build_function_hover(name, state, &self.stdlib_modules)
            }
            SymbolInfo::Struct { name } => build_struct_hover(name, state, &self.stdlib_modules),
            SymbolInfo::Constant { name } => {
                build_constant_hover(name, state, &self.stdlib_modules)
            }
            SymbolInfo::Enum { name } => build_enum_hover(name, state, &self.stdlib_modules),
            SymbolInfo::Protocol { name } => {
                build_protocol_hover(name, state, &self.stdlib_modules)
            }
            SymbolInfo::TypeAlias { name } => {
                build_type_alias_hover(name, state, &self.stdlib_modules)
            }
            SymbolInfo::Variable { name, type_display } => {
                let sig = match type_display {
                    Some(ty) => format!("{}: {}", name, ty),
                    None => name.to_string(),
                };
                Some(format!("```expo\n{}\n```", sig))
            }
        };

        match hover_text {
            Some(text) => Ok(Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: text,
                }),
                range: None,
            })),
            None => Ok(None),
        }
    }
}

/// Resolves a doc comment for `name` from the local module, sibling
/// project modules, or stdlib.
fn resolve_doc(name: &str, state: &DocumentState, stdlib_modules: &[Module]) -> Option<String> {
    lookup::find_doc_for(&state.module, name)
        .or_else(|| {
            state
                .project_modules
                .iter()
                .find_map(|m| lookup::find_doc_for(m, name))
        })
        .or_else(|| {
            stdlib_modules
                .iter()
                .find_map(|m| lookup::find_doc_for(m, name))
        })
}

fn build_function_hover(
    name: &str,
    state: &DocumentState,
    stdlib_modules: &[Module],
) -> Option<String> {
    let sig = state.ctx.functions.get(name)?;
    let params_str: Vec<String> = sig
        .params
        .iter()
        .map(|p| format!("{}: {}", p.name, p.ty.display()))
        .collect();
    let vis = if sig.visibility == expo_ast::ast::Visibility::Private {
        "priv fn"
    } else {
        "fn"
    };
    let tp = if sig.type_params.is_empty() {
        String::new()
    } else {
        format!(
            "<{}>",
            sig.type_params
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let signature = format!(
        "{} {}{}({}) -> {}",
        vis,
        name,
        tp,
        params_str.join(", "),
        sig.return_type.display()
    );
    let doc = resolve_doc(name, state, stdlib_modules);
    Some(format_hover(&signature, doc.as_deref()))
}

fn build_struct_hover(
    name: &str,
    state: &DocumentState,
    stdlib_modules: &[Module],
) -> Option<String> {
    let info = state.ctx.find_type(name)?;
    let fields: Vec<String> = info
        .fields()?
        .iter()
        .map(|(n, t)| format!("  {}: {}", n, t.display()))
        .collect();
    let tp = if info.type_params.is_empty() {
        String::new()
    } else {
        format!(
            "<{}>",
            info.type_params
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let signature = format!("struct {}{}\n{}\nend", name, tp, fields.join("\n"));
    let doc = resolve_doc(name, state, stdlib_modules);
    Some(format_hover(&signature, doc.as_deref()))
}

fn build_constant_hover(
    name: &str,
    state: &DocumentState,
    stdlib_modules: &[Module],
) -> Option<String> {
    let const_id = TypeIdentifier {
        package: state.ctx.current_package.clone()?,
        name: name.to_string(),
    };
    let ty = state.ctx.constants.get(&const_id)?;
    let signature = format!("const {}: {}", name, ty.display());
    let doc = resolve_doc(name, state, stdlib_modules);
    Some(format_hover(&signature, doc.as_deref()))
}

fn build_enum_hover(
    name: &str,
    state: &DocumentState,
    stdlib_modules: &[Module],
) -> Option<String> {
    let info = state.ctx.find_type(name)?;
    let variants: Vec<String> = info
        .variants()?
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
    let doc = resolve_doc(name, state, stdlib_modules);
    Some(format_hover(&signature, doc.as_deref()))
}

fn build_protocol_hover(
    name: &str,
    state: &DocumentState,
    stdlib_modules: &[Module],
) -> Option<String> {
    let info = state.ctx.protocols.get(name)?;
    let tp = if info.type_params.is_empty() {
        String::new()
    } else {
        format!(
            "<{}>",
            info.type_params
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let methods: Vec<String> = info
        .methods
        .iter()
        .map(|(n, sig)| {
            let params_str: Vec<String> = sig
                .params
                .iter()
                .map(|p| format!("{}: {}", p.name, p.ty.display()))
                .collect();
            format!(
                "  fn {}({}) -> {}",
                n,
                params_str.join(", "),
                sig.return_type.display()
            )
        })
        .collect();
    let signature = format!("protocol {}{}\n{}\nend", name, tp, methods.join("\n"));
    let doc = resolve_doc(name, state, stdlib_modules);
    Some(format_hover(&signature, doc.as_deref()))
}

fn build_type_alias_hover(
    name: &str,
    state: &DocumentState,
    stdlib_modules: &[Module],
) -> Option<String> {
    let ty = state.ctx.type_aliases.get(name)?;
    let signature = format!("type {} = {}", name, ty.display());
    let doc = resolve_doc(name, state, stdlib_modules);
    Some(format_hover(&signature, doc.as_deref()))
}

/// Formats a hover response as a Markdown code block with optional
/// documentation appended below a separator.
fn format_hover(signature: &str, doc: Option<&str>) -> String {
    let mut md = format!("```expo\n{}\n```", signature);
    if let Some(d) = doc {
        md.push_str("\n\n---\n\n");
        md.push_str(d);
    }
    md
}
