//! Coverage for list-literal resolution. `[a, b, c]` keeps its
//! `ExprKind::List` shape on the sealed AST with `expr.resolution =
//! List<T>` for the inferred element type; the desugar to a
//! `List.new().append(...)` chain happens at IR-lower time. These
//! tests confirm the literal's shape, the per-element resolutions,
//! and the bidirectional `List<T>` hint flow.

use expo_ast::ast::{Expr, ExprKind, Item, Literal, Statement};
use expo_ast::identifier::{Identifier, Resolution, ResolvedType};
use expo_ast::util::dedent;
use expo_typecheck::CheckedProgram;

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

fn assert_list_literal(expr: &Expr) -> &[Expr] {
    let ExprKind::List { elements } = &expr.kind else {
        panic!("expected ExprKind::List, got {:?}", expr.kind);
    };
    elements.as_slice()
}

#[test]
fn empty_list_literal_pins_element_from_binding_annotation() {
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
    let elements = assert_list_literal(value);
    assert!(elements.is_empty(), "empty `[]` carries no elements");
    assert_eq!(value.resolution, list_named_type(&checked, "Int"));
}

#[test]
fn nonempty_list_literal_resolves_each_element_in_source_order() {
    let source = "
        fn main
          [10, 20, 30]
        end
        ";
    let checked = typecheck(&dedent(source));
    let trailing = trailing_expr(&checked);
    let elements = assert_list_literal(trailing);
    assert_eq!(elements.len(), 3, "literal preserves all three elements");
    let digits: Vec<String> = elements
        .iter()
        .map(|element| match &element.kind {
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
fn nested_list_literals_keep_their_shape_bottom_up() {
    let source = "
        fn main
          [[1, 2], [3]]
        end
        ";
    let checked = typecheck(&dedent(source));
    let outer = trailing_expr(&checked);
    let outer_elements = assert_list_literal(outer);
    assert_eq!(outer_elements.len(), 2, "outer list has two elements");
    let inner_lengths: Vec<usize> = outer_elements
        .iter()
        .map(|element| assert_list_literal(element).len())
        .collect();
    assert_eq!(inner_lengths, vec![2, 1]);
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
    let elements = assert_list_literal(trailing);
    assert_eq!(elements.len(), 2);
    assert_eq!(trailing.resolution, list_named_type(&checked, "String"));
}
