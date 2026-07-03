//! Go-to-definition handler for the Koja LSP.
//!
//! Resolves the definition location for functions, structs, enums,
//! constants, protocols, type aliases, methods, and local variables.

use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;

use koja_ast::ast::File;
use koja_ast::identifier::Identifier;
use koja_ast::span::Span;
use koja_typecheck::GlobalRegistry;

use crate::backend::{Backend, DocumentState};
use crate::convert::{path_to_uri, span_to_range};
use crate::lookup::{self, LookupCtx, SymbolInfo};

impl Backend {
    /// Handles `textDocument/definition` requests by resolving the symbol
    /// under the cursor to its definition location.
    pub(crate) async fn handle_goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        let line = pos.line + 1;
        let col = pos.character + 1;

        let docs = self.documents.read().await;
        let state = match docs.get(uri.as_str()) {
            Some(s) => s,
            None => return Ok(None),
        };
        let (file, registry) = match (state.active_file(), state.registry()) {
            (Some(f), Some(r)) => (f, r),
            _ => return Ok(None),
        };
        let ctx = LookupCtx {
            registry,
            package: &state.active_package,
            locals: &state.locals,
        };

        let symbol = match lookup::find_symbol_at(file, line, col, &ctx) {
            Some(s) => s,
            None => return Ok(None),
        };

        // Resolve method symbols via the registry's `[Type, method]`
        // entry, which carries an authoritative defining span.
        if let SymbolInfo::Method {
            type_name,
            method_name,
        } = &symbol
            && let Some((span, target_pkg)) = lookup_method_span(type_name, method_name, registry)
        {
            return Ok(Some(resolve_location(&uri, span, &target_pkg, state)));
        }

        if let Some(name) = symbol_name(&symbol)
            && let Some((span, target_pkg)) =
                lookup_global_span(name, &state.active_package, registry)
        {
            return Ok(Some(resolve_location(&uri, span, &target_pkg, state)));
        }

        // Variable: jump to its declaring local span via the per-doc
        // index, matched by surface name (the symbol doesn't carry a
        // local id).
        if let SymbolInfo::Variable { name, .. } = &symbol
            && let Some(span) = find_local_span_by_name(state, name)
        {
            return Ok(Some(GotoDefinitionResponse::Scalar(Location {
                uri,
                range: span_to_range(&span),
            })));
        }

        Ok(None)
    }
}

fn symbol_name(symbol: &SymbolInfo) -> Option<&str> {
    Some(match symbol {
        SymbolInfo::Function { name }
        | SymbolInfo::Struct { name }
        | SymbolInfo::Enum { name }
        | SymbolInfo::Constant { name }
        | SymbolInfo::Protocol { name }
        | SymbolInfo::TypeAlias { name } => name.as_str(),
        SymbolInfo::Method { .. } | SymbolInfo::Variable { .. } => return None,
    })
}

fn lookup_global_span(
    name: &str,
    package: &str,
    registry: &GlobalRegistry,
) -> Option<(Span, String)> {
    for pkg in [package, "Global"] {
        let ident = Identifier::new(pkg, vec![name.to_string()]);
        if let Some((_, entry)) = registry.lookup(&ident) {
            return Some((entry.span, pkg.to_string()));
        }
    }
    None
}

fn lookup_method_span(
    type_name: &str,
    method_name: &str,
    registry: &GlobalRegistry,
) -> Option<(Span, String)> {
    for (_, entry) in registry.iter() {
        let path = entry.identifier.path();
        if path.len() == 2 && path[0] == type_name && path[1] == method_name {
            return Some((entry.span, entry.identifier.package().to_string()));
        }
    }
    None
}

/// Render a [`Location`] for the active or sibling file containing
/// `target_pkg`. We don't have the registry's span attributed to a
/// specific file path today, so we walk the document state's checked
/// packages to find which file's items declared the entry.
fn resolve_location(
    uri: &Uri,
    span: Span,
    target_pkg: &str,
    state: &DocumentState,
) -> GotoDefinitionResponse {
    let range = span_to_range(&span);
    if target_pkg == state.active_package {
        return GotoDefinitionResponse::Scalar(Location {
            uri: uri.clone(),
            range,
        });
    }
    let target_uri = find_pkg_file_uri(target_pkg, span, state).unwrap_or_else(|| uri.clone());
    GotoDefinitionResponse::Scalar(Location {
        uri: target_uri,
        range,
    })
}

fn find_pkg_file_uri(target_pkg: &str, span: Span, state: &DocumentState) -> Option<Uri> {
    let checked = state.checked.as_ref()?;
    for pkg in &checked.packages {
        if pkg.package != target_pkg {
            continue;
        }
        for file in &pkg.files {
            if file_contains_span(file, &span)
                && let Some(path) = &file.path
                && let Some(uri) = path_to_uri(path)
            {
                return Some(uri);
            }
        }
        if let Some(first) = pkg.files.first()
            && let Some(path) = &first.path
            && let Some(uri) = path_to_uri(path)
        {
            return Some(uri);
        }
    }
    None
}

/// Best-effort: a file "contains" a span when the span's start line
/// falls within the file's span range. Used to attribute a registry
/// span back to a concrete file path.
fn file_contains_span(file: &File, span: &Span) -> bool {
    span.start.line >= file.span.start.line && span.end.line <= file.span.end.line
}

/// Linear scan of the per-document local index for the first entry
/// whose name matches. The classify-by-name path doesn't carry a
/// [`LocalId`], so we look up by surface name. Per-file indices stay
/// small enough that the scan is unmeasurable.
fn find_local_span_by_name(state: &DocumentState, name: &str) -> Option<Span> {
    state
        .locals
        .iter()
        .find(|info| info.name == name)
        .map(|info| info.span)
}
