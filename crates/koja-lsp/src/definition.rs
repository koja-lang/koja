//! Go-to-definition handler for the Koja LSP.
//!
//! Resolves the definition location for functions, structs, enums,
//! constants, protocols, type aliases, methods, and local variables.

use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;

use koja_ast::ast::{Function, ImplMember, Item, TypeExpr};
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
            && let Some((span, identifier)) = lookup_method_span(type_name, method_name, registry)
        {
            return Ok(Some(resolve_location(&uri, span, &identifier, state)));
        }

        if let Some(name) = symbol_name(&symbol)
            && let Some((span, identifier)) =
                lookup_global_span(name, &state.active_package, registry)
        {
            return Ok(Some(resolve_location(&uri, span, &identifier, state)));
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
) -> Option<(Span, Identifier)> {
    for pkg in [package, "Global"] {
        let ident = Identifier::new(pkg, vec![name.to_string()]);
        if let Some((_, entry)) = registry.lookup(&ident) {
            return Some((entry.span, entry.identifier.clone()));
        }
    }
    None
}

fn lookup_method_span(
    type_name: &str,
    method_name: &str,
    registry: &GlobalRegistry,
) -> Option<(Span, Identifier)> {
    for (_, entry) in registry.iter() {
        let path = entry.identifier.path();
        if path.len() == 2 && path[0] == type_name && path[1] == method_name {
            return Some((entry.span, entry.identifier.clone()));
        }
    }
    None
}

/// Render a [`Location`] for the file declaring `identifier`. Registry
/// spans carry no file identity (they are file-local line numbers), so
/// we find the declaring file by name: the checked package whose AST
/// declares the identifier's path. Falls back to the active URI.
fn resolve_location(
    uri: &Uri,
    span: Span,
    identifier: &Identifier,
    state: &DocumentState,
) -> GotoDefinitionResponse {
    let target_uri = find_declaring_file_uri(identifier, state).unwrap_or_else(|| uri.clone());
    GotoDefinitionResponse::Scalar(Location {
        uri: target_uri,
        range: span_to_range(&span),
    })
}

fn find_declaring_file_uri(identifier: &Identifier, state: &DocumentState) -> Option<Uri> {
    let checked = state.checked.as_ref()?;
    for pkg in &checked.packages {
        if pkg.package != identifier.package() {
            continue;
        }
        for file in &pkg.files {
            if items_declare(&file.items, identifier.path())
                && let Some(path) = &file.path
                && let Some(uri) = path_to_uri(path)
            {
                return Some(uri);
            }
        }
    }
    None
}

/// Whether `items` contains the declaration the registry named `path`,
/// e.g. `["Option"]` or `["List", "append"]`.
fn items_declare(items: &[Item], path: &[String]) -> bool {
    items.iter().any(|item| item_declares(item, path))
}

fn item_declares(item: &Item, path: &[String]) -> bool {
    match item {
        Item::Alias(_) => false,
        Item::Constant(c) => declares_leaf(path, &c.name),
        Item::Function(f) => declares_leaf(path, &f.name),
        Item::TypeAlias(t) => declares_leaf(path, &t.name),
        Item::Protocol(p) => {
            path.first().map(String::as_str) == Some(p.name.as_str())
                && (path.len() == 1
                    || (path.len() == 2 && p.methods.iter().any(|m| m.name == path[1])))
        }
        Item::Struct(s) => type_declares(&s.path, &s.functions, &s.nested, path),
        Item::Enum(e) => type_declares(&e.path, &e.functions, &e.nested, path),
        Item::Impl(block) => block_declares(&block.target, &block.members, path),
        Item::Extend(block) => block_declares(&block.target, &block.members, path),
    }
}

fn declares_leaf(path: &[String], name: &str) -> bool {
    path.len() == 1 && path[0] == name
}

/// A struct/enum declares its own lexical path, its methods
/// (path plus function name), and whatever its nested types declare.
/// Nested decls carry only their leaf name at parse time (the owner
/// prefix is implied), so the recursion strips the matched prefix.
fn type_declares(
    type_path: &[String],
    functions: &[Function],
    nested: &[Item],
    path: &[String],
) -> bool {
    if path == type_path {
        return true;
    }
    if !path.starts_with(type_path) {
        return false;
    }
    if path.len() == type_path.len() + 1
        && functions.iter().any(|f| f.name == path[type_path.len()])
    {
        return true;
    }
    items_declare(nested, &path[type_path.len()..])
}

/// An `impl`/`extend` block declares its members at
/// `[target type path..., member name]`.
fn block_declares(target: &TypeExpr, members: &[ImplMember], path: &[String]) -> bool {
    let type_path = match target {
        TypeExpr::Named { path, .. } | TypeExpr::Generic { path, .. } => path,
        _ => return false,
    };
    path.len() == type_path.len() + 1
        && path.starts_with(type_path)
        && members.iter().any(|member| match member {
            ImplMember::Function(f) => f.name == path[type_path.len()],
            ImplMember::TypeAlias(t) => t.name == path[type_path.len()],
        })
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

#[cfg(test)]
mod tests {
    use super::*;
    use koja_ast::util::dedent;
    use koja_parser::ParseMode;

    fn parse_items(source: &str) -> Vec<Item> {
        let result = koja_parser::parse(&dedent(source), ParseMode::File);
        assert!(
            result.errors.is_empty(),
            "parse errors: {:?}",
            result.errors
        );
        result.ast.items
    }

    fn declares(source: &str, path: &[&str]) -> bool {
        let path: Vec<String> = path.iter().map(|s| s.to_string()).collect();
        items_declare(&parse_items(source), &path)
    }

    #[test]
    fn top_level_declarations_match_by_name() {
        let source = r#"
            const LIMIT: Int = 10

            enum Option<T>
              Some(T)
              None
            end

            fn helper() -> Int
              1
            end
        "#;
        assert!(declares(source, &["Option"]));
        assert!(declares(source, &["LIMIT"]));
        assert!(declares(source, &["helper"]));
        assert!(!declares(source, &["Result"]));
    }

    #[test]
    fn methods_match_through_type_bodies_and_extend_blocks() {
        let source = r#"
            struct Point
              x: Int

              fn origin() -> Self
                Point{x: 0}
              end
            end

            extend List<T>
              fn second(self) -> Option<T>
                self.get(1)
              end
            end
        "#;
        assert!(declares(source, &["Point", "origin"]));
        assert!(declares(source, &["List", "second"]));
        assert!(!declares(source, &["Point", "translate"]));
        assert!(!declares(source, &["List"]));
    }

    #[test]
    fn nested_types_match_their_full_path() {
        let source = r#"
            struct Process
              enum Step
                Continue
                Done
              end
            end
        "#;
        assert!(declares(source, &["Process", "Step"]));
        assert!(!declares(source, &["Step"]));
    }
}
