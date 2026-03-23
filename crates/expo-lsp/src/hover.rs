//! Hover information provider for the Expo LSP.
//!
//! Builds rich hover content (signatures and doc comments) for functions,
//! structs, enums, constants, modules, and variables.

use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;

use expo_ast::ast::{AnnotationValue, Module};
use expo_typecheck::context::VariantData;

use crate::backend::{Backend, DocumentState};
use crate::convert::{find_doc_from_uri, resolve_module_file};
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
            SymbolInfo::Constant { name } => build_constant_hover(name, state),
            SymbolInfo::Enum { name } => build_enum_hover(name, state, &self.stdlib_modules),
            SymbolInfo::ModuleFunction { module, name } => {
                build_module_function_hover(module, name, state)
            }
            SymbolInfo::Variable { name } => Some(format!("```expo\n{}\n```", name)),
            SymbolInfo::Module { path } => Some(build_module_hover(path, &uri, state)),
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

/// Resolves a doc comment for `name`, checking imported origins first,
/// then the local module, then all stdlib modules.
fn resolve_doc(name: &str, state: &DocumentState, stdlib_modules: &[Module]) -> Option<String> {
    if let Some(origin_uri) = state.imported_origins.get(name) {
        find_doc_from_uri(origin_uri, name)
    } else {
        lookup::find_doc_for(&state.module, name).or_else(|| {
            stdlib_modules
                .iter()
                .find_map(|m| lookup::find_doc_for(m, name))
        })
    }
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
    let doc = resolve_doc(name, state, stdlib_modules);
    Some(format_hover(&signature, doc.as_deref()))
}

fn build_struct_hover(
    name: &str,
    state: &DocumentState,
    stdlib_modules: &[Module],
) -> Option<String> {
    let info = state.ctx.types.get(name)?;
    let fields: Vec<String> = info
        .fields()?
        .iter()
        .map(|(n, t)| format!("  {}: {}", n, t.display()))
        .collect();
    let tp = if info.type_params.is_empty() {
        String::new()
    } else {
        format!("<{}>", info.type_params.join(", "))
    };
    let signature = format!("struct {}{}\n{}\nend", name, tp, fields.join("\n"));
    let doc = resolve_doc(name, state, stdlib_modules);
    Some(format_hover(&signature, doc.as_deref()))
}

fn build_constant_hover(name: &str, state: &DocumentState) -> Option<String> {
    let ty = state.ctx.constants.get(name)?;
    let signature = format!("const {}: {}", name, ty.display());
    let doc = lookup::find_doc_for(&state.module, name);
    Some(format_hover(&signature, doc.as_deref()))
}

fn build_enum_hover(
    name: &str,
    state: &DocumentState,
    stdlib_modules: &[Module],
) -> Option<String> {
    let info = state.ctx.types.get(name)?;
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

fn build_module_function_hover(module: &str, name: &str, state: &DocumentState) -> Option<String> {
    let sig = state
        .ctx
        .imported_modules
        .get(module)
        .and_then(|m| m.functions.get(name))?;
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
    Some(format_hover(&signature, doc.as_deref()))
}

fn build_module_hover(path: &[String], uri: &Uri, state: &DocumentState) -> String {
    let module_name = path.join(".");
    let signature = format!("module {}", module_name);
    let doc = state
        .module_uris
        .get(&module_name)
        .and_then(|uri_str| {
            let p = uri_str.strip_prefix("file://")?;
            std::fs::read_to_string(p).ok()
        })
        .or_else(|| resolve_module_file(uri, path).and_then(|p| std::fs::read_to_string(p).ok()))
        .and_then(|source| {
            let parsed = expo_parser::parse(&source);
            parsed.module.moduledoc.and_then(|d| match d.value {
                Some(AnnotationValue::String(s)) => Some(s),
                _ => None,
            })
        });
    format_hover(&signature, doc.as_deref())
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
