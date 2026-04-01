//! Symbol lookup and classification for the Expo LSP.
//!
//! Provides the core symbol-finding API used by hover and go-to-definition
//! handlers: given a cursor position, determine which symbol (if any) is
//! under it.

mod span;
mod traverse;

use expo_ast::ast::*;
use expo_typecheck::context::TypeContext;

use span::{span_contains, span_contains_name};
use traverse::{find_in_ident_at_name, find_in_statement, find_in_type_expr};

/// Describes the kind and identity of a symbol found at a cursor position.
#[derive(Debug)]
pub(crate) enum SymbolInfo {
    Constant { name: String },
    Enum { name: String },
    Function { name: String },
    Struct { name: String },
    Variable { name: String },
}

/// Finds the symbol at the given 1-indexed `(line, col)` position in
/// a parsed module.
pub(crate) fn find_symbol_at(
    module: &Module,
    line: u32,
    col: u32,
    ctx: &TypeContext,
) -> Option<SymbolInfo> {
    for item in &module.items {
        match item {
            Item::Function(f) => {
                if !span_contains(&f.span, line, col) {
                    continue;
                }
                if let Some(info) = find_in_ident_at_name(&f.name, &f.span, line, col, ctx) {
                    return Some(info);
                }
                for stmt in &f.body {
                    if let Some(info) = find_in_statement(stmt, line, col, ctx) {
                        return Some(info);
                    }
                }
            }
            Item::Impl(imp) => {
                for member in &imp.members {
                    if let ImplMember::Function(f) = member {
                        if !span_contains(&f.span, line, col) {
                            continue;
                        }
                        for stmt in &f.body {
                            if let Some(info) = find_in_statement(stmt, line, col, ctx) {
                                return Some(info);
                            }
                        }
                    }
                }
            }
            Item::Protocol(p) => {
                if !span_contains(&p.span, line, col) {
                    continue;
                }
                for m in &p.methods {
                    if let Some(body) = &m.body {
                        if !span_contains(&m.span, line, col) {
                            continue;
                        }
                        for stmt in body {
                            if let Some(info) = find_in_statement(stmt, line, col, ctx) {
                                return Some(info);
                            }
                        }
                    }
                }
            }
            Item::Struct(s) => {
                if span_contains_name(&s.name, &s.span, line, col) {
                    return Some(SymbolInfo::Struct {
                        name: s.name.clone(),
                    });
                }
            }
            Item::Enum(e) => {
                if span_contains_name(&e.name, &e.span, line, col) {
                    return Some(SymbolInfo::Enum {
                        name: e.name.clone(),
                    });
                }
            }
            Item::Constant(c) => {
                if span_contains(&c.span, line, col) {
                    if let Some(type_ann) = &c.type_annotation
                        && let Some(info) = find_in_type_expr(type_ann, line, col, ctx)
                    {
                        return Some(info);
                    }
                    if span_contains_name(&c.name, &c.span, line, col) {
                        return Some(SymbolInfo::Constant {
                            name: c.name.clone(),
                        });
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// Searches a module's items for the `@doc` annotation on the item
/// named `name`.
pub(crate) fn find_doc_for(module: &Module, name: &str) -> Option<String> {
    for item in &module.items {
        match item {
            Item::Function(f) if f.name == name => {
                return span::annotation_doc(&f.annotation);
            }
            Item::Struct(s) if s.name == name => {
                return span::annotation_doc(&s.annotation);
            }
            Item::Enum(e) if e.name == name => {
                return span::annotation_doc(&e.annotation);
            }
            Item::Constant(c) if c.name == name => {
                return span::annotation_doc(&c.annotation);
            }
            Item::TypeAlias(t) if t.name == name => {
                return span::annotation_doc(&t.annotation);
            }
            Item::Impl(imp) => {
                for member in &imp.members {
                    if let ImplMember::Function(f) = member {
                        if f.name == name {
                            return span::annotation_doc(&f.annotation);
                        }
                        let impl_type_name = match &imp.target {
                            TypeExpr::Named { path, .. } | TypeExpr::Generic { path, .. } => {
                                path.last().map(|s| s.as_str())
                            }
                            _ => None,
                        };
                        let mangled = impl_type_name
                            .map(|t| format!("{t}_{}", f.name))
                            .unwrap_or_default();
                        if mangled == name {
                            return span::annotation_doc(&f.annotation);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// Classifies an identifier by looking it up in the type context,
/// returning the appropriate [`SymbolInfo`] variant.
pub(crate) fn classify_name(name: &str, ctx: &TypeContext) -> Option<SymbolInfo> {
    if ctx.functions.contains_key(name) {
        Some(SymbolInfo::Function {
            name: name.to_string(),
        })
    } else if ctx.is_struct(name) {
        Some(SymbolInfo::Struct {
            name: name.to_string(),
        })
    } else if ctx.is_enum(name) {
        Some(SymbolInfo::Enum {
            name: name.to_string(),
        })
    } else if ctx.constants.contains_key(name) {
        Some(SymbolInfo::Constant {
            name: name.to_string(),
        })
    } else {
        Some(SymbolInfo::Variable {
            name: name.to_string(),
        })
    }
}
