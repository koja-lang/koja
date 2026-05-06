//! Typecheck coverage for string literal resolution. Plain strings
//! type as `Global.String`; interpolation surfaces a feature-gap
//! diagnostic but the resolver still walks the interpolated
//! expressions so seal sees a populated tree.

use expo_alpha_typecheck::CheckedProgram;
use expo_ast::ast::{Expr, ExprKind, Function, Item, Statement};
use expo_ast::identifier::{Identifier, Resolution, ResolvedType};
use expo_ast::util::dedent;

mod common;

use common::{
    PACKAGE, diagnostic_messages, typecheck_file as typecheck,
    typecheck_file_fail as typecheck_fail,
};

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
fn string_interpolation_diagnoses() {
    let source = "
        fn greeting -> String
          \"hello #{1}\"
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages.iter().any(|m| m.contains("string interpolation")),
        "expected interpolation diagnostic, got {messages:?}",
    );
}
