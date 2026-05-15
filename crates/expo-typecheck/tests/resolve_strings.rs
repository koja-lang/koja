//! Typecheck coverage for string literal resolution. Plain and
//! interpolated strings both resolve to `Global.String`. The
//! resolver wraps non-`String` interpolation expressions in a
//! synthetic `.format()` MethodCall so IR-lower sees a `String`
//! per part; `String`-typed expressions are left bare to avoid the
//! quote-adding behavior of `String.format`.

use expo_alpha_typecheck::CheckedProgram;
use expo_ast::ast::{Expr, ExprKind, Function, Item, Statement, StringPart};
use expo_ast::identifier::{Identifier, Resolution, ResolvedType};
use expo_ast::util::dedent;

mod common;

use common::{PACKAGE, diagnostic_messages, parse_and_check, typecheck_file as typecheck};
use expo_parser::ParseMode;

fn find_function<'a>(checked: &'a CheckedProgram, name: &str) -> &'a Function {
    let pkg = checked
        .packages
        .iter()
        .find(|p| p.package == PACKAGE)
        .expect("checked program is missing the test package");
    for file in &pkg.files {
        for item in &file.items {
            if let Item::Function(function) = item
                && function.name == name
            {
                return function;
            }
        }
    }
    panic!("fn {name} not found in checked program");
}

fn trailing_expr(function: &Function) -> &Expr {
    let body = function
        .body
        .as_deref()
        .expect("function has no body (extern?)");
    let trailing = body.last().expect("expected at least one statement");
    match trailing {
        Statement::Expr(expr) => expr,
        other => panic!("expected Statement::Expr as trailing statement, got {other:?}"),
    }
}

fn global_leaf(checked: &CheckedProgram, name: &str) -> ResolvedType {
    let ident = Identifier::new("Global", vec![name.to_string()]);
    let (id, _) = checked
        .registry
        .lookup(&ident)
        .unwrap_or_else(|| panic!("stdlib stub `Global.{name}` missing from registry"));
    ResolvedType::leaf(Resolution::Global(id))
}

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
    let greeting = find_function(&checked, "greeting");
    let trailing = trailing_expr(greeting);
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
    let greeting = find_function(&checked, "greeting");
    let trailing = trailing_expr(greeting);
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
    let greeting = find_function(&checked, "greeting");
    let trailing = trailing_expr(greeting);
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
    let greeting = find_function(&checked, "greeting");
    let trailing = trailing_expr(greeting);
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
    let render = find_function(&checked, "render");
    let trailing = trailing_expr(render);
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
    // `Unit` has no `impl Debug for Unit` in `lib/global/src/debug.expo`,
    // so wrapping a Unit-typed interp in `.format()` surfaces the
    // standard method-lookup diagnostic from the call resolver.
    let source = "
        priv fn returns_unit
        end

        priv fn render -> String
          \"x = #{returns_unit()}\"
        end
        ";

    let failure = parse_and_check(&dedent(source), ParseMode::File)
        .expect_err("expected typecheck to fail when the interp receiver has no `format` method");
    let messages = diagnostic_messages(&failure);
    assert!(
        messages.iter().any(|m| m.contains("format")),
        "expected a method-lookup diagnostic mentioning `format`, got {messages:?}",
    );
}
