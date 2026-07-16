//! Typecheck coverage for string literal resolution. Plain and
//! interpolated strings both resolve to `Global.String`. The
//! resolver wraps non-`String` interpolation expressions in a
//! synthetic `.format()` MethodCall so IR-lower sees a `String`
//! per part. `String`-typed expressions are left bare to avoid the
//! quote-adding behavior of `String.format`.

use koja_ast::ast::{Expr, ExprKind, StringPart};
use koja_ast::util::dedent;

mod common;

use common::{
    assert_script_fails_with, function_body, global_leaf, last_expr, typecheck_script as typecheck,
};

fn interpolation_exprs(string_expr: &Expr) -> Vec<&Expr> {
    let ExprKind::String { parts, .. } = &string_expr.kind else {
        panic!("expected ExprKind::String, got {:?}", string_expr.kind);
    };
    parts
        .iter()
        .filter_map(|p| match p {
            StringPart::Interpolation { expr, .. } => Some(expr.as_ref()),
            StringPart::Literal { .. } => None,
        })
        .collect()
}

#[test]
fn string_literal_resolves_to_global_string() {
    let source = "
        fn greeting -> String
          \"hello\"
        end
        ";

    let checked = typecheck(&dedent(source));
    let string = global_leaf(&checked, "String");
    let trailing = last_expr(function_body(&checked, "greeting"));
    assert_eq!(trailing.resolution, string);
    assert!(matches!(trailing.kind, ExprKind::String { .. }));
}

#[test]
fn string_interpolation_with_explicit_format_left_unchanged() {
    let source = "
        fn greeting -> String
          \"hello #{1.format()}\"
        end
        ";

    let checked = typecheck(&dedent(source));
    let string = global_leaf(&checked, "String");
    let trailing = last_expr(function_body(&checked, "greeting"));
    assert_eq!(trailing.resolution, string);
    let interps = interpolation_exprs(trailing);
    assert_eq!(interps.len(), 1);
    assert_eq!(interps[0].resolution, string);
    let ExprKind::MethodCall {
        method, receiver, ..
    } = &interps[0].kind
    else {
        panic!(
            "expected the user-written `.format()` to remain a MethodCall, got {:?}",
            interps[0].kind,
        );
    };
    assert_eq!(method, "format");
    assert!(
        matches!(receiver.kind, ExprKind::Literal { .. }),
        "expected `1.format()` receiver to be a literal, got {:?}",
        receiver.kind,
    );
}

#[test]
fn string_interpolation_int_wraps_in_format() {
    let source = "
        fn greeting -> String
          \"n = #{42}\"
        end
        ";

    let checked = typecheck(&dedent(source));
    let string = global_leaf(&checked, "String");
    let trailing = last_expr(function_body(&checked, "greeting"));
    assert_eq!(trailing.resolution, string);
    let interps = interpolation_exprs(trailing);
    assert_eq!(interps.len(), 1);
    let ExprKind::MethodCall {
        method, receiver, ..
    } = &interps[0].kind
    else {
        panic!(
            "expected resolver to wrap the bare `42` in a MethodCall, got {:?}",
            interps[0].kind,
        );
    };
    assert_eq!(method, "format");
    assert_eq!(interps[0].resolution, string);
    let ExprKind::Literal { .. } = &receiver.kind else {
        panic!(
            "expected wrapped receiver to be the original Int literal, got {:?}",
            receiver.kind,
        );
    };
    let int = global_leaf(&checked, "Int");
    assert_eq!(receiver.resolution, int);
}

#[test]
fn string_interpolation_string_left_unwrapped() {
    let source = "
        fn greeting -> String
          name = \"alice\"
          \"hello #{name}\"
        end
        ";

    let checked = typecheck(&dedent(source));
    let string = global_leaf(&checked, "String");
    let trailing = last_expr(function_body(&checked, "greeting"));
    assert_eq!(trailing.resolution, string);
    let interps = interpolation_exprs(trailing);
    assert_eq!(interps.len(), 1);
    assert_eq!(interps[0].resolution, string);
    let ExprKind::Ident { name, .. } = &interps[0].kind else {
        panic!(
            "expected String-typed interp to stay a bare Ident, got {:?}",
            interps[0].kind,
        );
    };
    assert_eq!(name, "name");
}

#[test]
fn string_interpolation_struct_wraps_in_format() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn render -> String
          p = Point{x: 1, y: 2}
          \"point is #{p}\"
        end
        ";

    let checked = typecheck(&dedent(source));
    let string = global_leaf(&checked, "String");
    let trailing = last_expr(function_body(&checked, "render"));
    assert_eq!(trailing.resolution, string);
    let interps = interpolation_exprs(trailing);
    assert_eq!(interps.len(), 1);
    let ExprKind::MethodCall {
        method, receiver, ..
    } = &interps[0].kind
    else {
        panic!(
            "expected resolver to wrap struct interp in a `.format()` MethodCall, got {:?}",
            interps[0].kind,
        );
    };
    assert_eq!(method, "format");
    assert_eq!(interps[0].resolution, string);
    assert!(
        matches!(receiver.kind, ExprKind::Ident { .. }),
        "expected receiver to be the original `p` Ident, got {:?}",
        receiver.kind,
    );
}

#[test]
fn string_interpolation_without_format_method_diagnoses() {
    // Function types have no `Debug` impl, so wrapping a
    // closure-typed interp in `.format()` surfaces the standard
    // receiver-shape diagnostic from the call resolver.
    let source = "
        priv fn render -> String
          f = fn (x: Int) -> Int
            x
          end
          \"f = #{f}\"
        end
        ";

    assert_script_fails_with(source, &["receiver must have a struct or enum type"]);
}
