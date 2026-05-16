//! Hover information provider for the Expo LSP.
//!
//! Builds rich hover content (signatures and doc comments) for functions,
//! structs, enums, constants, protocols, type aliases, and variables.

use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;

use expo_ast::ast::File;
use expo_ast::identifier::Identifier;
use expo_typecheck::{FunctionSignature, GlobalKind, GlobalRegistry};

use crate::backend::{Backend, DocumentState};
use crate::format::{
    format_enum_def, format_function_signature, format_protocol_def, format_resolved_type,
    format_struct_def,
};
use crate::lookup::{self, LookupCtx, SymbolInfo};

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

        let (active_file, registry) = match (state.active_file(), state.registry()) {
            (Some(f), Some(r)) => (f, r),
            _ => return Ok(None),
        };

        let ctx = LookupCtx {
            registry,
            package: &state.active_package,
            locals: &state.locals,
        };

        let symbol = match lookup::find_symbol_at(active_file, line, col, &ctx) {
            Some(s) => s,
            None => return Ok(None),
        };

        let stdlib_files = collect_stdlib_files(state);

        let hover_text = match &symbol {
            SymbolInfo::Function { name } => {
                build_function_hover(name, &state.active_package, state, registry, &stdlib_files)
            }
            SymbolInfo::Method {
                type_name,
                method_name,
            } => build_method_hover(type_name, method_name, state, registry, &stdlib_files),
            SymbolInfo::Struct { name } => {
                build_struct_hover(name, &state.active_package, state, registry, &stdlib_files)
            }
            SymbolInfo::Constant { name } => {
                build_constant_hover(name, &state.active_package, state, registry, &stdlib_files)
            }
            SymbolInfo::Enum { name } => {
                build_enum_hover(name, &state.active_package, state, registry, &stdlib_files)
            }
            SymbolInfo::Protocol { name } => {
                build_protocol_hover(name, &state.active_package, state, registry, &stdlib_files)
            }
            SymbolInfo::TypeAlias { name } => {
                build_type_alias_hover(name, &state.active_package, state, registry, &stdlib_files)
            }
            SymbolInfo::Variable { name, type_display } => {
                let sig = match type_display {
                    Some(ty) => format!("{name}: {ty}"),
                    None => name.to_string(),
                };
                Some(format!("```expo\n{sig}\n```"))
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

fn collect_stdlib_files(state: &DocumentState) -> Vec<&File> {
    let mut out: Vec<&File> = Vec::new();
    if let Some(checked) = &state.checked {
        for pkg in &checked.packages {
            for file in &pkg.files {
                if file.path.as_deref() != Some(state.active_path.as_path()) {
                    out.push(file);
                }
            }
        }
    }
    out
}

fn resolve_doc(name: &str, state: &DocumentState, stdlib_files: &[&File]) -> Option<String> {
    state
        .active_file()
        .and_then(|f| lookup::find_doc_for(f, name))
        .or_else(|| {
            stdlib_files
                .iter()
                .find_map(|f| lookup::find_doc_for(f, name))
        })
}

/// Look up a registry entry by name, searching the active package
/// first and then `Global`. Returns the matched [`Identifier`]
/// alongside `(GlobalKind, type_params)` so callers can render
/// type-parameter lists without re-walking the entry.
fn lookup_global<'a>(
    name: &str,
    package: &str,
    registry: &'a GlobalRegistry,
) -> Option<(Identifier, &'a GlobalKind, &'a [String])> {
    for pkg in [package, "Global"] {
        let ident = Identifier::new(pkg, vec![name.to_string()]);
        if let Some((_, entry)) = registry.lookup(&ident) {
            return Some((ident, &entry.kind, &entry.type_params));
        }
    }
    None
}

fn build_function_hover(
    name: &str,
    package: &str,
    state: &DocumentState,
    registry: &GlobalRegistry,
    stdlib_files: &[&File],
) -> Option<String> {
    let (_, kind, type_params) = lookup_global(name, package, registry)?;
    let GlobalKind::Function(Some(sig)) = kind else {
        return None;
    };
    let signature = format_function_signature(name, sig, type_params, registry);
    let doc = resolve_doc(name, state, stdlib_files);
    Some(format_hover(&signature, doc.as_deref()))
}

fn build_method_hover(
    type_name: &str,
    method_name: &str,
    state: &DocumentState,
    registry: &GlobalRegistry,
    stdlib_files: &[&File],
) -> Option<String> {
    let (sig, _owner_pkg, type_params) = find_method_signature(type_name, method_name, registry)?;
    let display_name = format!("{type_name}.{method_name}");
    let signature = format_function_signature(&display_name, sig, type_params, registry);
    let mangled = format!("{type_name}_{method_name}");
    let doc = resolve_doc(&mangled, state, stdlib_files)
        .or_else(|| resolve_doc(method_name, state, stdlib_files));
    Some(format_hover(&signature, doc.as_deref()))
}

fn find_method_signature<'a>(
    type_name: &str,
    method_name: &str,
    registry: &'a GlobalRegistry,
) -> Option<(&'a FunctionSignature, String, &'a [String])> {
    for (_, entry) in registry.iter() {
        let path = entry.identifier.path();
        if path.len() == 2
            && path[0] == type_name
            && path[1] == method_name
            && let GlobalKind::Function(Some(sig)) = &entry.kind
        {
            return Some((
                sig,
                entry.identifier.package().to_string(),
                &entry.type_params,
            ));
        }
    }
    None
}

fn build_struct_hover(
    name: &str,
    package: &str,
    state: &DocumentState,
    registry: &GlobalRegistry,
    stdlib_files: &[&File],
) -> Option<String> {
    let (_, kind, type_params) = lookup_global(name, package, registry)?;
    let GlobalKind::Struct(Some(def)) = kind else {
        return None;
    };
    let signature = format_struct_def(name, type_params, &def.fields, registry);
    let doc = resolve_doc(name, state, stdlib_files);
    Some(format_hover(&signature, doc.as_deref()))
}

fn build_constant_hover(
    name: &str,
    package: &str,
    state: &DocumentState,
    registry: &GlobalRegistry,
    stdlib_files: &[&File],
) -> Option<String> {
    let (_, kind, _) = lookup_global(name, package, registry)?;
    let GlobalKind::Constant(Some(def)) = kind else {
        return None;
    };
    let signature = format!("const {name}: {}", format_resolved_type(&def.ty, registry));
    let doc = resolve_doc(name, state, stdlib_files);
    Some(format_hover(&signature, doc.as_deref()))
}

fn build_enum_hover(
    name: &str,
    package: &str,
    state: &DocumentState,
    registry: &GlobalRegistry,
    stdlib_files: &[&File],
) -> Option<String> {
    let (_, kind, type_params) = lookup_global(name, package, registry)?;
    let GlobalKind::Enum(Some(def)) = kind else {
        return None;
    };
    let signature = format_enum_def(name, type_params, &def.variants, registry);
    let doc = resolve_doc(name, state, stdlib_files);
    Some(format_hover(&signature, doc.as_deref()))
}

fn build_protocol_hover(
    name: &str,
    package: &str,
    state: &DocumentState,
    registry: &GlobalRegistry,
    stdlib_files: &[&File],
) -> Option<String> {
    let (_, kind, type_params) = lookup_global(name, package, registry)?;
    let GlobalKind::Protocol(Some(def)) = kind else {
        return None;
    };
    let signature = format_protocol_def(name, type_params, &def.methods, registry);
    let doc = resolve_doc(name, state, stdlib_files);
    Some(format_hover(&signature, doc.as_deref()))
}

fn build_type_alias_hover(
    name: &str,
    package: &str,
    state: &DocumentState,
    registry: &GlobalRegistry,
    stdlib_files: &[&File],
) -> Option<String> {
    let (_, kind, _) = lookup_global(name, package, registry)?;
    let GlobalKind::TypeAlias(Some(expansion)) = kind else {
        return None;
    };
    let signature = format!(
        "type {name} = {}",
        format_resolved_type(expansion, registry)
    );
    let doc = resolve_doc(name, state, stdlib_files);
    Some(format_hover(&signature, doc.as_deref()))
}

/// Render the hover body as a Markdown code block with optional
/// documentation appended below a separator.
fn format_hover(signature: &str, doc: Option<&str>) -> String {
    let mut md = format!("```expo\n{signature}\n```");
    if let Some(d) = doc {
        md.push_str("\n\n---\n\n");
        md.push_str(d);
    }
    md
}
