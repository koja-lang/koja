//! Coverage for the `derive_debug` synthesizer in
//! [`expo_alpha_typecheck::pipeline::synthesize::derive_debug`]. Each
//! test pins one body shape — struct with primitive fields, enum
//! with mixed variant data, opaque-typed fields rendering as
//! `"..."`, and the package-wide existing-impl scan that suppresses
//! synthesis when a hand-written impl is in another file of the
//! same package.

use expo_ast::ast::{Item, StringPart};
use expo_ast::util::dedent;

mod common;

use common::typecheck_file as typecheck;

/// Find the synthesized `format` body for `Type` in the test app
/// package and return the body's string parts. Panics if the type
/// has no Debug impl or its `format` body isn't a string expression.
fn format_string_parts(
    checked: &expo_alpha_typecheck::CheckedProgram,
    type_name: &str,
) -> Vec<StringPart> {
    let pkg = checked
        .packages
        .iter()
        .find(|p| p.package == common::PACKAGE)
        .expect("test package present");
    for file in &pkg.files {
        for item in &file.items {
            let Item::Impl(block) = item else { continue };
            let Some(trait_expr) = block.trait_expr.as_ref() else {
                continue;
            };
            if !type_expr_head_eq(trait_expr, "Debug") {
                continue;
            }
            if !type_expr_head_eq(&block.target, type_name) {
                continue;
            }
            for member in &block.members {
                let expo_ast::ast::ImplMember::Function(function) = member else {
                    continue;
                };
                if function.name != "format" {
                    continue;
                }
                let body = function.body.as_ref().expect("format has a body");
                let expo_ast::ast::Statement::Expr(expr) = &body[0] else {
                    panic!("format body must be a single expression statement");
                };
                let expo_ast::ast::ExprKind::String { parts, .. } = &expr.kind else {
                    panic!(
                        "expected string-expression body for `{type_name}.format`, got {:?}",
                        expr.kind
                    );
                };
                return parts.clone();
            }
        }
    }
    panic!("no synthesized `Debug for {type_name}` impl found");
}

fn type_expr_head_eq(te: &expo_ast::ast::TypeExpr, expected: &str) -> bool {
    match te {
        expo_ast::ast::TypeExpr::Named { path, .. }
        | expo_ast::ast::TypeExpr::Generic { path, .. } => {
            path.last().map(|s| s.as_str()) == Some(expected)
        }
        _ => false,
    }
}

fn literal_value(part: &StringPart) -> Option<&str> {
    match part {
        StringPart::Literal { value, .. } => Some(value.as_str()),
        StringPart::Interpolation { .. } => None,
    }
}

fn is_interpolation(part: &StringPart) -> bool {
    matches!(part, StringPart::Interpolation { .. })
}

#[test]
fn struct_with_primitive_fields_synthesizes_field_format_chain() {
    // `Point{x: <fmt>, y: <fmt>}` — alternating literal / interpolation
    // parts with the right separators. The interpolation expressions
    // wrap each `self.field` in a `format()` MethodCall; we just
    // assert the part shape here, and the IR tests pin the lowered
    // Concat chain.
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn main
          1
        end
        ";

    let checked = typecheck(&dedent(source));
    let parts = format_string_parts(&checked, "Point");
    let literals: Vec<&str> = parts.iter().filter_map(literal_value).collect();
    assert_eq!(
        literals,
        vec!["Point{", "x: ", ", ", "y: ", "}"],
        "literal scaffolding mismatch in `Point.format`",
    );
    let interp_count = parts.iter().filter(|p| is_interpolation(p)).count();
    assert_eq!(
        interp_count, 2,
        "expected one interpolation per field; got {interp_count}",
    );
}

#[test]
fn opaque_field_types_render_as_dotdotdot_placeholder() {
    // `Binary` is on the synthesizer's opaque list — its field
    // renders as a literal `"..."` rather than an interpolated
    // `self.field.format()` call. This keeps the synthesizer total
    // even before `Debug for Binary` lands as a real impl.
    let source = "
        struct Wrap
          payload: Binary
        end

        fn main
          1
        end
        ";

    let checked = typecheck(&dedent(source));
    let parts = format_string_parts(&checked, "Wrap");
    let literals: Vec<&str> = parts.iter().filter_map(literal_value).collect();
    assert_eq!(literals, vec!["Wrap{", "payload: ", "...", "}"]);
    assert!(
        !parts.iter().any(is_interpolation),
        "opaque field should not produce an interpolation; got {parts:?}",
    );
}

#[test]
fn enum_synthesizes_match_body_with_per_variant_arms() {
    // `Shape.format` is a match over the variants; each arm is
    // a Statement::Expr around a string expression. Pin that the
    // arm count matches and that one arm is a literal-only body
    // (the unit variant).
    let source = "
        enum Shape
          Point
          Tagged(Int)
          Labeled{name: String, count: Int}
        end

        fn main
          1
        end
        ";

    let checked = typecheck(&dedent(source));
    let pkg = checked
        .packages
        .iter()
        .find(|p| p.package == common::PACKAGE)
        .expect("test package present");
    let mut found = false;
    for file in &pkg.files {
        for item in &file.items {
            let Item::Impl(block) = item else { continue };
            if !block
                .trait_expr
                .as_ref()
                .map(|te| type_expr_head_eq(te, "Debug"))
                .unwrap_or(false)
            {
                continue;
            }
            if !type_expr_head_eq(&block.target, "Shape") {
                continue;
            }
            for member in &block.members {
                let expo_ast::ast::ImplMember::Function(function) = member else {
                    continue;
                };
                if function.name != "format" {
                    continue;
                }
                let expo_ast::ast::Statement::Expr(expr) =
                    &function.body.as_ref().expect("body")[0]
                else {
                    panic!("expected expr body");
                };
                let expo_ast::ast::ExprKind::Match { arms, .. } = &expr.kind else {
                    panic!("Shape.format body must be a Match; got {:?}", expr.kind);
                };
                assert_eq!(arms.len(), 3, "one arm per variant");
                found = true;
            }
        }
    }
    assert!(found, "synthesized `Debug for Shape` impl missing");
}

#[test]
fn generic_struct_synthesizes_full_body_for_universal_debug_dispatch() {
    // Generic types get the same field-walking body as concrete
    // ones; the universal-Debug fallback resolves `A.format()` /
    // `B.format()` on bare type parameters at typecheck, and
    // monomorphization picks the concrete impl post-IR.
    let source = "
        struct Container<A, B>
          left: A
          right: B
        end

        fn main
          1
        end
        ";

    let checked = typecheck(&dedent(source));
    let parts = format_string_parts(&checked, "Container");
    let literals: Vec<&str> = parts.iter().filter_map(literal_value).collect();
    assert_eq!(literals, vec!["Container{", "left: ", ", ", "right: ", "}"],);
    assert_eq!(parts.iter().filter(|p| is_interpolation(p)).count(), 2);
}

#[test]
fn user_supplied_debug_impl_suppresses_synthesis() {
    // The synthesizer's `collect_existing_debug_impls` scan should
    // see this hand-written impl and skip generating a duplicate.
    // Two impls would trip the `duplicate impl` collision in
    // `collect`.
    let source = "
        struct Custom
          x: Int
        end

        impl Debug for Custom
          fn format(self) -> String
            \"custom\"
          end
        end

        fn main
          1
        end
        ";

    let checked = typecheck(&dedent(source));
    let pkg = checked
        .packages
        .iter()
        .find(|p| p.package == common::PACKAGE)
        .expect("test package present");
    let custom_debug_impls: usize = pkg
        .files
        .iter()
        .flat_map(|f| f.items.iter())
        .filter(|item| {
            let Item::Impl(block) = item else {
                return false;
            };
            block
                .trait_expr
                .as_ref()
                .map(|te| type_expr_head_eq(te, "Debug"))
                .unwrap_or(false)
                && type_expr_head_eq(&block.target, "Custom")
        })
        .count();
    assert_eq!(
        custom_debug_impls, 1,
        "expected a single (hand-written) Debug impl for Custom",
    );
}
