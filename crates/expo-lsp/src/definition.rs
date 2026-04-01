//! Go-to-definition handler for the Expo LSP.
//!
//! Resolves the definition location for functions, structs, enums,
//! constants, protocols, and type aliases across the current file,
//! sibling project files, and stdlib.

use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;

use expo_ast::ast::{ImplMember, Item, Module, TypeExpr};
use expo_ast::span::Span;

use crate::backend::Backend;
use crate::convert::{path_to_uri, span_to_range};
use crate::lookup::{self, SymbolInfo};

/// Searches a module's items for the definition of `name`, returning
/// its span if found.
fn find_definition_in_module(module: &Module, name: &str) -> Option<Span> {
    for item in &module.items {
        match item {
            Item::Function(f) if f.name == name => return Some(f.span),
            Item::Struct(s) if s.name == name => return Some(s.span),
            Item::Enum(e) if e.name == name => return Some(e.span),
            Item::Constant(c) if c.name == name => return Some(c.span),
            Item::Protocol(p) if p.name == name => return Some(p.span),
            Item::TypeAlias(t) if t.name == name => return Some(t.span),
            Item::Impl(imp) => {
                let impl_type_name = match &imp.target {
                    TypeExpr::Named { path, .. } | TypeExpr::Generic { path, .. } => {
                        path.last().map(|s| s.as_str())
                    }
                    _ => None,
                };
                for member in &imp.members {
                    if let ImplMember::Function(f) = member {
                        if f.name == name {
                            return Some(f.span);
                        }
                        if let Some(t) = impl_type_name
                            && format!("{}_{}", t, f.name) == name
                        {
                            return Some(f.span);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    None
}

impl Backend {
    /// Handles `textDocument/definition` requests by resolving the symbol
    /// under the cursor to its definition location, searching the current
    /// file, sibling project files, and stdlib.
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

        let symbol = match lookup::find_symbol_at(&state.module, line, col, &state.ctx) {
            Some(s) => s,
            None => return Ok(None),
        };

        let name = match &symbol {
            SymbolInfo::Function { name }
            | SymbolInfo::Struct { name }
            | SymbolInfo::Enum { name }
            | SymbolInfo::Constant { name }
            | SymbolInfo::Protocol { name }
            | SymbolInfo::TypeAlias { name } => name.as_str(),
            SymbolInfo::Variable { .. } => return Ok(None),
        };

        if let Some(span) = find_definition_in_module(&state.module, name) {
            return Ok(Some(GotoDefinitionResponse::Scalar(Location {
                uri: uri.clone(),
                range: span_to_range(&span),
            })));
        }

        for module in &state.project_modules {
            if let Some(span) = find_definition_in_module(module, name) {
                let target_uri = module
                    .path
                    .as_deref()
                    .and_then(path_to_uri)
                    .unwrap_or_else(|| uri.clone());
                return Ok(Some(GotoDefinitionResponse::Scalar(Location {
                    uri: target_uri,
                    range: span_to_range(&span),
                })));
            }
        }

        for module in &self.stdlib_modules {
            if let Some(span) = find_definition_in_module(module, name) {
                let target_uri = module
                    .path
                    .as_deref()
                    .and_then(path_to_uri)
                    .unwrap_or_else(|| uri.clone());
                return Ok(Some(GotoDefinitionResponse::Scalar(Location {
                    uri: target_uri,
                    range: span_to_range(&span),
                })));
            }
        }

        Ok(None)
    }
}
