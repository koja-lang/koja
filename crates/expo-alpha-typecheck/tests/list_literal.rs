//! Coverage for the list-literal desugar in
//! `pipeline::synthesize::list_literal_desugar` and its downstream
//! resolution. Confirms `[a, b, c]` collapses to
//! `List.new().append(a).append(b).append(c)`, that the trailing
//! expression resolves to `List<T>` for the inferred element type,
//! and that nested literals collapse bottom-up.

use expo_alpha_typecheck::CheckedProgram;
use expo_ast::ast::{Arg, Expr, ExprKind, Item, Literal, Statement};
use expo_ast::identifier::{Identifier, Resolution, ResolvedType};
use expo_ast::util::dedent;

mod common;

use common::{PACKAGE, typecheck_file as typecheck};

fn main_body(checked: &CheckedProgram) -> &[Statement] {
    let pkg = checked
        .packages
        .iter()
        .find(|p| p.package == PACKAGE)
        .expect("checked program is missing the test package");
    let file = pkg.files.first().expect("package has no files");
    let main = file
        .items
        .iter()
        .find_map(|item| match item {
            Item::Function(function) if function.name == "main" => Some(function),
            _ => None,
        })
        .expect("file is missing `fn main`");
    main.body
        .as_deref()
        .expect("`fn main` has no body — extern fn cannot be the entry point")
}

fn trailing_expr(checked: &CheckedProgram) -> &Expr {
    let body = main_body(checked);
    let trailing = body.last().expect("expected at least one statement");
    match trailing {
        Statement::Expr(expr) => expr,
        other => panic!("expected Statement::Expr as trailing statement, got {other:?}"),
    }
}

fn list_named_type(checked: &CheckedProgram, element: &str) -> ResolvedType {
    let list_ident = Identifier::new("Global", vec!["List".to_string()]);
    let (list_id, _) = checked
        .registry
        .lookup(&list_ident)
        .unwrap_or_else(|| panic!("autoimported `Global.List` missing from registry"));
    let element_ident = Identifier::new("Global", vec![element.to_string()]);
    let (element_id, _) = checked
        .registry
        .lookup(&element_ident)
        .unwrap_or_else(|| panic!("stdlib stub `Global.{element}` missing from registry"));
    ResolvedType::Named {
        resolution: Resolution::Global(list_id),
        type_args: vec![ResolvedType::leaf(Resolution::Global(element_id))],
    }
}

/// `[a, b, c]` desugars to a left-folded `append` chain rooted at
/// `List.new()`. Walks the [`ExprKind::MethodCall`] spine from the
/// outermost call back to the `List.new()` seed and returns the
/// element args in source order.
fn collect_append_chain(expr: &Expr) -> (&Expr, Vec<&Arg>) {
    let mut args = Vec::new();
    let mut current = expr;
    loop {
        let ExprKind::MethodCall {
            receiver,
            method,
            args: call_args,
            ..
        } = &current.kind
        else {
            panic!("expected MethodCall, got {current:?}");
        };
        if method == "new" {
            return (current, args.into_iter().rev().collect());
        }
        assert_eq!(method, "append", "list-literal chain unexpected method");
        assert_eq!(
            call_args.len(),
            1,
            "append should carry exactly one arg per literal element",
        );
        args.push(&call_args[0]);
        current = receiver;
    }
}

#[test]
fn empty_list_literal_desugars_to_bare_new_call() {
    let source = "
        fn main
          my_list: List<Int> = []
          my_list
        end
        ";
    let checked = typecheck(&dedent(source));
    let body = main_body(&checked);
    let assignment = body.first().expect("missing assignment");
    let Statement::Assignment { value, .. } = assignment else {
        panic!("expected Statement::Assignment, got {assignment:?}");
    };
    let ExprKind::MethodCall { method, args, .. } = &value.kind else {
        panic!("empty list should desugar to a single `List.new()` MethodCall");
    };
    assert_eq!(method, "new", "empty `[]` should reduce to `List.new()`");
    assert!(args.is_empty(), "`List.new()` takes no args");
    assert_eq!(value.resolution, list_named_type(&checked, "Int"));
}

#[test]
fn nonempty_list_literal_chains_appends_in_source_order() {
    let source = "
        fn main
          [10, 20, 30]
        end
        ";
    let checked = typecheck(&dedent(source));
    let trailing = trailing_expr(&checked);
    let (_, args) = collect_append_chain(trailing);
    assert_eq!(args.len(), 3, "expected one append per literal element");
    let digits: Vec<String> = args
        .iter()
        .map(|arg| match &arg.value.kind {
            ExprKind::Literal {
                value: Literal::Int(digits),
            } => digits.clone(),
            other => panic!("expected Int literal element, got {other:?}"),
        })
        .collect();
    assert_eq!(digits, vec!["10", "20", "30"]);
    assert_eq!(trailing.resolution, list_named_type(&checked, "Int"));
}

#[test]
fn nested_list_literals_collapse_bottom_up() {
    let source = "
        fn main
          [[1, 2], [3]]
        end
        ";
    let checked = typecheck(&dedent(source));
    let outer = trailing_expr(&checked);
    let (_, outer_args) = collect_append_chain(outer);
    assert_eq!(outer_args.len(), 2, "outer list has two elements");
    for (index, outer_arg) in outer_args.iter().enumerate() {
        let (_, inner_args) = collect_append_chain(&outer_arg.value);
        let expected_len = if index == 0 { 2 } else { 1 };
        assert_eq!(
            inner_args.len(),
            expected_len,
            "inner list #{index} should desugar to {expected_len} appends",
        );
    }
}

#[test]
fn list_literal_with_string_elements_resolves_to_list_string() {
    let source = "
        fn main
          [\"a\", \"b\"]
        end
        ";
    let checked = typecheck(&dedent(source));
    let trailing = trailing_expr(&checked);
    assert_eq!(trailing.resolution, list_named_type(&checked, "String"));
}
