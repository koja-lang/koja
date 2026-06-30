//! Symbol lookup and classification for the Koja LSP.
//!
//! Provides the core symbol-finding API used by hover and go-to-definition
//! handlers: given a cursor position, determine which symbol (if any) is
//! under it.

mod local_index;
mod span;
mod traverse;

use koja_ast::ast::*;
use koja_ast::identifier::Identifier;
use koja_typecheck::{GlobalKind, GlobalRegistry};

pub(crate) use local_index::LocalIndex;
pub(crate) use span::span_contains;
use span::span_contains_name;
pub(crate) use traverse::{
    find_enclosing_call, find_expr_at, receiver_type_id as traverse_receiver_type_id,
};
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
    /// owning type's name and the bare method name.
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
        /// Resolved type rendered for display, if available.
        type_display: Option<String>,
    },
}

/// Bundle of resolved-state inputs the lookup helpers need to classify
/// names against the type registry. Keeps every traversal signature a
/// single `&LookupCtx` instead of threading three slots per call.
#[derive(Clone, Copy)]
pub(crate) struct LookupCtx<'a> {
    pub(crate) registry: &'a GlobalRegistry,
    pub(crate) package: &'a str,
    pub(crate) locals: &'a LocalIndex,
}

/// Finds the symbol at the given 1-indexed `(line, col)` position in
/// a parsed file.
pub(crate) fn find_symbol_at(
    file: &File,
    line: u32,
    col: u32,
    ctx: &LookupCtx<'_>,
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
                if span_contains_name(s.name(), &s.span, line, col) {
                    return Some(SymbolInfo::Struct {
                        name: s.name().to_string(),
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
                if span_contains_name(e.name(), &e.span, line, col) {
                    return Some(SymbolInfo::Enum {
                        name: e.name().to_string(),
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
    ctx: &LookupCtx<'_>,
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
///   the bare name (`puts`) and the mangled `Type_method` form.
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
                if s.name() == name {
                    return span::annotation_doc(&s.annotations);
                }
                if let Some(doc) = doc_in_methods(&s.functions, s.name(), name) {
                    return Some(doc);
                }
            }
            Item::Enum(e) => {
                if e.name() == name {
                    return span::annotation_doc(&e.annotations);
                }
                if let Some(doc) = doc_in_methods(&e.functions, e.name(), name) {
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
                if let Some(doc) = doc_in_block_members(&imp.target, &imp.members, name) {
                    return Some(doc);
                }
            }
            Item::Extend(ext) => {
                if let Some(doc) = doc_in_block_members(&ext.target, &ext.members, name) {
                    return Some(doc);
                }
            }
            _ => {}
        }
    }
    None
}

/// Helper for `find_doc_for`: looks up a function inside a list of
/// inline methods on a struct or enum.
fn doc_in_methods(functions: &[Function], type_name: &str, name: &str) -> Option<String> {
    for f in functions {
        if f.name == name || format!("{type_name}_{}", f.name) == name {
            return span::annotation_doc(&f.annotations);
        }
    }
    None
}

/// Shared helper for `Item::Impl` and `Item::Extend`: match a
/// member's bare name (`bump`) or its mangled form (`Counter_bump`).
fn doc_in_block_members(target: &TypeExpr, members: &[ImplMember], name: &str) -> Option<String> {
    let target_name = match target {
        TypeExpr::Named { path, .. } | TypeExpr::Generic { path, .. } => {
            path.last().map(|s| s.as_str())
        }
        _ => None,
    };
    for member in members {
        if let ImplMember::Function(f) = member
            && (f.name == name
                || target_name
                    .map(|t| format!("{t}_{}", f.name) == name)
                    .unwrap_or(false))
        {
            return span::annotation_doc(&f.annotations);
        }
    }
    None
}

/// Classifies an identifier by looking it up in the type registry,
/// returning the appropriate [`SymbolInfo`] variant. Looks first in
/// the active package, then falls back to `Global`. Unknown names
/// classify as [`SymbolInfo::Variable`].
pub(crate) fn classify_name(name: &str, ctx: &LookupCtx<'_>) -> Option<SymbolInfo> {
    if let Some(info) = classify_in_package(name, ctx.package, ctx.registry) {
        return Some(info);
    }
    if ctx.package != "Global"
        && let Some(info) = classify_in_package(name, "Global", ctx.registry)
    {
        return Some(info);
    }
    Some(SymbolInfo::Variable {
        name: name.to_string(),
        type_display: None,
    })
}

fn classify_in_package(name: &str, package: &str, registry: &GlobalRegistry) -> Option<SymbolInfo> {
    let identifier = Identifier::new(package, vec![name.to_string()]);
    let (_, entry) = registry.lookup(&identifier)?;
    Some(match &entry.kind {
        GlobalKind::Function(_) => SymbolInfo::Function {
            name: name.to_string(),
        },
        GlobalKind::Struct(_) => SymbolInfo::Struct {
            name: name.to_string(),
        },
        GlobalKind::Enum(_) => SymbolInfo::Enum {
            name: name.to_string(),
        },
        GlobalKind::Protocol(_) => SymbolInfo::Protocol {
            name: name.to_string(),
        },
        GlobalKind::Constant(_) => SymbolInfo::Constant {
            name: name.to_string(),
        },
        GlobalKind::TypeAlias(_) => SymbolInfo::TypeAlias {
            name: name.to_string(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use koja_ast::util::dedent;
    use koja_parser::{ParseMode, SourceFile, parse_program};
    use koja_typecheck::{CheckedProgram, check_program};
    use std::path::PathBuf;

    const PACKAGE: &str = "TestApp";

    fn check(source: &str) -> CheckedProgram {
        let mut sources = koja_stdlib::autoimport_sources();
        sources.extend(koja_stdlib::qualified_sources());
        sources.push(SourceFile {
            package: PACKAGE.to_string(),
            path: PathBuf::from("test.koja"),
            source: dedent(source),
        });
        let parsed = parse_program(sources, ParseMode::File);
        check_program(parsed)
            .unwrap_or_else(|f| panic!("typecheck failed: {} diagnostic(s)", f.diagnostics.len()))
    }

    fn parse_source(source: &str) -> File {
        let result = koja_parser::parse(&dedent(source), ParseMode::File);
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
            fn add(a: Int, b: Int) -> Int
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

            extend Counter
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

    #[test]
    fn classify_resolves_top_level_function() {
        let checked = check(
            r#"
            fn add(a: Int, b: Int) -> Int
              a + b
            end
            "#,
        );
        let locals = LocalIndex::default();
        let ctx = LookupCtx {
            registry: &checked.registry,
            package: PACKAGE,
            locals: &locals,
        };
        let info = classify_name("add", &ctx).expect("classify");
        assert!(matches!(info, SymbolInfo::Function { ref name } if name == "add"));
    }

    #[test]
    fn classify_resolves_struct_from_active_package() {
        let checked = check(
            r#"
            struct Point
              x: Int
              y: Int
            end
            "#,
        );
        let locals = LocalIndex::default();
        let ctx = LookupCtx {
            registry: &checked.registry,
            package: PACKAGE,
            locals: &locals,
        };
        let info = classify_name("Point", &ctx).expect("classify");
        assert!(matches!(info, SymbolInfo::Struct { ref name } if name == "Point"));
    }

    /// Smoke test: classify_name surfaces stdlib `Int` even though
    /// the active package is the user's.
    #[test]
    fn classify_resolves_global_primitive_fallback() {
        let checked = check("fn id(x: Int) -> Int\n  x\nend\n");
        let locals = LocalIndex::default();
        let ctx = LookupCtx {
            registry: &checked.registry,
            package: PACKAGE,
            locals: &locals,
        };
        let info = classify_name("Int", &ctx).expect("classify");
        assert!(matches!(info, SymbolInfo::Struct { ref name } if name == "Int"));
    }

    /// Smoke test mirroring the hover/definition pipeline: build a
    /// `CheckedProgram`, find the symbol at the cursor on a function
    /// name's line. Exercises `find_symbol_at` end-to-end against the
    /// type registry.
    #[test]
    fn find_symbol_at_resolves_function_name() {
        let checked = check(
            r#"
            fn greet() -> Unit
              ()
            end
            "#,
        );
        let active_path = PathBuf::from("test.koja");
        let file = checked
            .packages
            .iter()
            .find(|p| p.package == PACKAGE)
            .and_then(|pkg| {
                pkg.files
                    .iter()
                    .find(|f| f.path.as_deref() == Some(active_path.as_path()))
            })
            .expect("active file in checked program");
        let locals = LocalIndex::default();
        let ctx = LookupCtx {
            registry: &checked.registry,
            package: PACKAGE,
            locals: &locals,
        };
        // Cursor on the `greet` name (line 1, col 5 — between `f`/`n` and parens).
        let info = find_symbol_at(file, 1, 5, &ctx).expect("symbol at cursor");
        assert!(matches!(info, SymbolInfo::Function { ref name } if name == "greet"));
    }

    /// Smoke test mirroring the definition pipeline's local-index
    /// path: every function param + body local lands in the index.
    #[test]
    fn local_index_records_params_and_body_locals() {
        let mut sources = koja_stdlib::autoimport_sources();
        sources.extend(koja_stdlib::qualified_sources());
        let active = PathBuf::from("test.koja");
        sources.push(SourceFile {
            package: PACKAGE.to_string(),
            path: active.clone(),
            source: dedent(
                r#"
                fn add(a: Int, b: Int) -> Int
                  total = a + b
                  total
                end
                "#,
            ),
        });
        let parsed = parse_program(sources, ParseMode::File);
        let _ = check_program(parsed).expect("check passes");

        // Rebuild parsed for the LocalIndex (check_program consumes its
        // input; mirror diagnostics.rs::rebuild_parsed_from_checked).
        let mut sources = koja_stdlib::autoimport_sources();
        sources.extend(koja_stdlib::qualified_sources());
        sources.push(SourceFile {
            package: PACKAGE.to_string(),
            path: active.clone(),
            source: dedent(
                r#"
                fn add(a: Int, b: Int) -> Int
                  total = a + b
                  total
                end
                "#,
            ),
        });
        let parsed = parse_program(sources, ParseMode::File);
        let checked = check_program(parsed).expect("check passes");

        // Round-trip parsed via the checked program so we exercise the
        // same shape the diagnostics pipeline hands LocalIndex::build.
        let mut rebuilt_files = std::collections::BTreeMap::new();
        let mut order = Vec::new();
        for pkg in &checked.packages {
            for file in &pkg.files {
                let path = file
                    .path
                    .clone()
                    .unwrap_or_else(|| PathBuf::from(format!("<{}>", pkg.package)));
                order.push(path.clone());
                rebuilt_files.insert(
                    path.clone(),
                    koja_parser::ParsedFile {
                        ast: file.clone(),
                        diagnostics: Vec::new(),
                        package: pkg.package.clone(),
                        path,
                        source: String::new(),
                    },
                );
            }
        }
        let rebuilt = koja_parser::ParsedProgram {
            files: rebuilt_files,
            order,
        };
        let idx = LocalIndex::build(&rebuilt, &active);
        let names: std::collections::BTreeSet<String> =
            idx.iter().map(|info| info.name.clone()).collect();
        assert!(names.contains("a"), "expected `a` in {:?}", names);
        assert!(names.contains("b"), "expected `b` in {:?}", names);
        assert!(names.contains("total"), "expected `total` in {:?}", names);
    }
}
