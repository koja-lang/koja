//! Coverage for list-literal resolution. `[a, b, c]` keeps its
//! `ExprKind::List` shape on the sealed AST with `expr.resolution =
//! List<T>` for the inferred element type. The desugar to a
//! `List.new().append(...)` chain happens at IR-lower time. These
//! tests confirm the literal's shape, the per-element resolutions,
//! and the bidirectional `List<T>` hint flow.

use koja_ast::ast::{Expr, ExprKind, Literal, Statement};
use koja_ast::identifier::ResolvedType;
use koja_ast::util::dedent;
use koja_typecheck::CheckedProgram;

mod common;

use common::{
    global_leaf, global_named, script_body, trailing_expr, typecheck_script as typecheck,
};

fn list_named_type(checked: &CheckedProgram, element: &str) -> ResolvedType {
    global_named(checked, "List", vec![global_leaf(checked, element)])
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
        my_list: List<Int> = []
        my_list
        ";
    let checked = typecheck(&dedent(source));
    let body = script_body(&checked);
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
        [10, 20, 30]
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
        [[1, 2], [3]]
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
        [\"a\", \"b\"]
        ";
    let checked = typecheck(&dedent(source));
    let trailing = trailing_expr(&checked);
    let elements = assert_list_literal(trailing);
    assert_eq!(elements.len(), 2);
    assert_eq!(trailing.resolution, list_named_type(&checked, "String"));
}
