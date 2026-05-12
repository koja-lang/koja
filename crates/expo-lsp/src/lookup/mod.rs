//! Symbol lookup and classification for the Expo LSP.
//!
//! Provides the core symbol-finding API used by hover and go-to-definition
//! handlers: given a cursor position, determine which symbol (if any) is
//! under it.

mod span;
mod traverse;

use expo_ast::ast::*;
use expo_ast::identifier::TypeIdentifier;
use expo_typecheck::context::TypeContext;

use span::{span_contains, span_contains_name};
pub(crate) use traverse::{find_enclosing_call, find_expr_at};
use traverse::{find_in_ident_at_name, find_in_params, find_in_statement, find_in_type_expr};

/// Describes the kind and identity of a symbol found at a cursor position.
#[derive(Debug)]
pub(crate) enum SymbolInfo {
    Constant {
        name: String,
    },
    Enum {
        name: String,
    },
    Function {
        name: String,
    },
    /// A method on a struct, enum, or protocol. Carries both the
    /// owning type's name and the bare method name so the hover can
    /// look up the function signature on the type's `TypeInfo` and
    /// the doc string under the mangled `Type_method` form.
    Method {
        type_name: String,
        method_name: String,
    },
    Protocol {
        name: String,
    },
    Struct {
        name: String,
    },
    TypeAlias {
        name: String,
    },
    Variable {
        name: String,
        /// Human-readable type string from `resolved_type`, if available.
        type_display: Option<String>,
    },
}

/// Finds the symbol at the given 1-indexed `(line, col)` position in
/// a parsed file.
pub(crate) fn find_symbol_at(
    file: &File,
    line: u32,
    col: u32,
    ctx: &TypeContext,
) -> Option<SymbolInfo> {
    for item in &file.items {
        match item {
            Item::Alias(_) => {}
            Item::Function(f) => {
                if !span_contains(&f.span, line, col) {
                    continue;
                }
                if let Some(info) = find_in_ident_at_name(&f.name, &f.span, line, col, ctx) {
                    return Some(info);
                }
                if let Some(info) = find_in_params(&f.params, line, col, ctx) {
                    return Some(info);
                }
                if let Some(ret) = &f.return_type
                    && let Some(info) = find_in_type_expr(ret, line, col, ctx)
                {
                    return Some(info);
                }
                if let Some(body) = &f.body {
                    for stmt in body {
                        if let Some(info) = find_in_statement(stmt, line, col, ctx) {
                            return Some(info);
                        }
                    }
                }
            }
            Item::Impl(imp) => {
                for member in &imp.members {
                    if let ImplMember::Function(f) = member {
                        if !span_contains(&f.span, line, col) {
                            continue;
                        }
                        if let Some(info) = find_in_params(&f.params, line, col, ctx) {
                            return Some(info);
                        }
                        if let Some(ret) = &f.return_type
                            && let Some(info) = find_in_type_expr(ret, line, col, ctx)
                        {
                            return Some(info);
                        }
                        if let Some(body) = &f.body {
                            for stmt in body {
                                if let Some(info) = find_in_statement(stmt, line, col, ctx) {
                                    return Some(info);
                                }
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
                    if !span_contains(&m.span, line, col) {
                        continue;
                    }
                    if let Some(body) = &m.body {
                        for stmt in body {
                            if let Some(info) = find_in_statement(stmt, line, col, ctx) {
                                return Some(info);
                            }
                        }
                    }
                }
            }
            Item::Struct(s) => {
                if !span_contains(&s.span, line, col) {
                    continue;
                }
                if span_contains_name(&s.name, &s.span, line, col) {
                    return Some(SymbolInfo::Struct {
                        name: s.name.clone(),
                    });
                }
                for field in &s.fields {
                    if let Some(info) = find_in_type_expr(&field.type_expr, line, col, ctx) {
                        return Some(info);
                    }
                }
                if let Some(info) = find_in_inline_functions(&s.functions, line, col, ctx) {
                    return Some(info);
                }
            }
            Item::Enum(e) => {
                if !span_contains(&e.span, line, col) {
                    continue;
                }
                if span_contains_name(&e.name, &e.span, line, col) {
                    return Some(SymbolInfo::Enum {
                        name: e.name.clone(),
                    });
                }
                for variant in &e.variants {
                    if let EnumVariantData::Struct(fields) = &variant.data {
                        for field in fields {
                            if let Some(info) = find_in_type_expr(&field.type_expr, line, col, ctx)
                            {
                                return Some(info);
                            }
                        }
                    }
                    if let EnumVariantData::Tuple(types) = &variant.data {
                        for te in types {
                            if let Some(info) = find_in_type_expr(te, line, col, ctx) {
                                return Some(info);
                            }
                        }
                    }
                }
                if let Some(info) = find_in_inline_functions(&e.functions, line, col, ctx) {
                    return Some(info);
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

/// Searches inline functions inside a struct or enum body for a symbol at position.
fn find_in_inline_functions(
    functions: &[Function],
    line: u32,
    col: u32,
    ctx: &TypeContext,
) -> Option<SymbolInfo> {
    for f in functions {
        if !span_contains(&f.span, line, col) {
            continue;
        }
        if let Some(info) = find_in_params(&f.params, line, col, ctx) {
            return Some(info);
        }
        if let Some(ret) = &f.return_type
            && let Some(info) = find_in_type_expr(ret, line, col, ctx)
        {
            return Some(info);
        }
        if let Some(body) = &f.body {
            for stmt in body {
                if let Some(info) = find_in_statement(stmt, line, col, ctx) {
                    return Some(info);
                }
            }
        }
    }
    None
}

/// Searches a file's items for the `@doc` annotation on the item
/// named `name`. Handles three families of names:
///
/// * Top-level declarations (`fn`, `struct`, `enum`, `const`,
///   `protocol`, `type`).
/// * Inline methods on `struct` / `enum` declarations: matches both
///   the bare name (`puts`) and the mangled `Type_method` form used
///   by static-call hovers (`IO_puts`).
/// * Methods inside `impl` blocks (same dual form) and default
///   methods on `protocol` declarations.
pub(crate) fn find_doc_for(file: &File, name: &str) -> Option<String> {
    for item in &file.items {
        match item {
            Item::Alias(_) => {}
            Item::Function(f) if f.name == name => {
                return span::annotation_doc(&f.annotations);
            }
            Item::Struct(s) => {
                if s.name == name {
                    return span::annotation_doc(&s.annotations);
                }
                if let Some(doc) = doc_in_methods(&s.functions, &s.name, name) {
                    return Some(doc);
                }
            }
            Item::Enum(e) => {
                if e.name == name {
                    return span::annotation_doc(&e.annotations);
                }
                if let Some(doc) = doc_in_methods(&e.functions, &e.name, name) {
                    return Some(doc);
                }
            }
            Item::Constant(c) if c.name == name => {
                return span::annotation_doc(&c.annotations);
            }
            Item::Protocol(p) => {
                if p.name == name {
                    return span::annotation_doc(&p.annotations);
                }
                for method in &p.methods {
                    if method.name == name || format!("{}_{}", p.name, method.name) == name {
                        return span::annotation_doc(&method.annotations);
                    }
                }
            }
            Item::TypeAlias(t) if t.name == name => {
                return span::annotation_doc(&t.annotations);
            }
            Item::Impl(imp) => {
                let impl_type_name = match &imp.target {
                    TypeExpr::Named { path, .. } | TypeExpr::Generic { path, .. } => {
                        path.last().map(|s| s.as_str())
                    }
                    _ => None,
                };
                for member in &imp.members {
                    if let ImplMember::Function(f) = member
                        && (f.name == name
                            || impl_type_name
                                .map(|t| format!("{t}_{}", f.name) == name)
                                .unwrap_or(false))
                    {
                        return span::annotation_doc(&f.annotations);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// Helper for `find_doc_for`: looks up a function inside a list of
/// inline methods on a struct or enum, matching either the bare
/// method name or the mangled `Type_method` form.
fn doc_in_methods(functions: &[Function], type_name: &str, name: &str) -> Option<String> {
    for f in functions {
        if f.name == name || format!("{type_name}_{}", f.name) == name {
            return span::annotation_doc(&f.annotations);
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
    } else if ctx.protocols.contains_key(name) {
        Some(SymbolInfo::Protocol {
            name: name.to_string(),
        })
    } else if ctx.current_package.as_ref().is_some_and(|pkg| {
        ctx.constants.contains_key(&TypeIdentifier {
            package: pkg.clone(),
            name: name.to_string(),
        })
    }) {
        Some(SymbolInfo::Constant {
            name: name.to_string(),
        })
    } else if ctx.type_aliases.contains_key(name) {
        Some(SymbolInfo::TypeAlias {
            name: name.to_string(),
        })
    } else {
        Some(SymbolInfo::Variable {
            name: name.to_string(),
            type_display: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use expo_ast::util::dedent;
    use expo_parser::{ParseMode, parse};

    fn parse_source(source: &str) -> File {
        let result = parse(&dedent(source), ParseMode::File);
        assert!(
            result.errors.is_empty(),
            "parse errors: {:?}",
            result.errors
        );
        result.ast
    }

    #[test]
    fn finds_doc_on_top_level_function() {
        let file = parse_source(
            r#"
            @doc """
            Adds two numbers.
            """
            fn add(a: I32, b: I32) -> I32
              a + b
            end
            "#,
        );
        assert!(
            find_doc_for(&file, "add")
                .unwrap()
                .contains("Adds two numbers.")
        );
    }

    #[test]
    fn finds_doc_on_inline_struct_method_via_mangled_name() {
        let file = parse_source(
            r#"
            struct Greeter
              @doc """
              Says hello.
              """
              fn hello()
              end
            end
            "#,
        );
        assert!(
            find_doc_for(&file, "Greeter_hello")
                .unwrap()
                .contains("Says hello.")
        );
    }

    #[test]
    fn finds_doc_on_protocol_default_method() {
        let file = parse_source(
            r#"
            protocol Greet
              @doc """
              Greeting verb.
              """
              fn hello(self) -> String
            end
            "#,
        );
        assert!(
            find_doc_for(&file, "Greet_hello")
                .unwrap()
                .contains("Greeting verb.")
        );
        assert!(
            find_doc_for(&file, "hello")
                .unwrap()
                .contains("Greeting verb.")
        );
    }

    #[test]
    fn finds_doc_on_impl_method_via_mangled_name() {
        let file = parse_source(
            r#"
            struct Counter
            end

            impl Counter
              @doc """
              Increments the counter.
              """
              fn bump(self)
              end
            end
            "#,
        );
        assert!(
            find_doc_for(&file, "Counter_bump")
                .unwrap()
                .contains("Increments the counter.")
        );
    }
}
