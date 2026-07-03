//! Coverage for set-literal resolution. `Set<T>` conforms to
//! `Global.ListLiteral<T>`, so `[a, b, c]` with an expected type of
//! `Set<T>` is rewritten in-place at resolve time into a
//! `Set.from_list([a, b, c])` method call. The inner literal keeps
//! `ExprKind::List` and stamps `List<T>`. The outer rewritten node
//! stamps `Set<T>` and dispatches through the normal method-call
//! resolver.

use koja_ast::ast::{ExprKind, Statement};
use koja_ast::identifier::{Identifier, Resolution, ResolvedType};
use koja_ast::util::dedent;
use koja_typecheck::CheckedProgram;

mod common;

use common::{PACKAGE, typecheck_script as typecheck};

fn body_statements(checked: &CheckedProgram) -> &[Statement] {
    let pkg = checked
        .packages
        .iter()
        .find(|p| p.package == PACKAGE)
        .expect("checked program is missing the test package");
    let file = pkg.files.first().expect("package has no files");
    file.body
        .as_deref()
        .expect("script-mode file must keep statements on File.body")
}

fn set_named_type(checked: &CheckedProgram, element: &str) -> ResolvedType {
    let set_ident = Identifier::new("Global", vec!["Set".to_string()]);
    let (set_id, _) = checked
        .registry
        .lookup(&set_ident)
        .unwrap_or_else(|| panic!("autoimported `Global.Set` missing from registry"));
    let element_ident = Identifier::new("Global", vec![element.to_string()]);
    let (element_id, _) = checked
        .registry
        .lookup(&element_ident)
        .unwrap_or_else(|| panic!("stdlib stub `Global.{element}` missing from registry"));
    ResolvedType::Named {
        resolution: Resolution::Global(set_id),
        type_args: vec![ResolvedType::leaf(Resolution::Global(element_id))],
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

#[test]
fn set_literal_with_int_elements_synthesizes_from_list_call() {
    let source = "
        numbers: Set<Int> = [1, 2, 3]
        numbers
        ";
    let checked = typecheck(&dedent(source));
    let body = body_statements(&checked);
    let assignment = body.first().expect("missing assignment");
    let Statement::Assignment { value, .. } = assignment else {
        panic!("expected Statement::Assignment, got {assignment:?}");
    };
    assert_eq!(value.resolution, set_named_type(&checked, "Int"));
    let ExprKind::MethodCall {
        receiver,
        method,
        args,
        ..
    } = &value.kind
    else {
        panic!(
            "expected `[1, 2, 3]` to be rewritten as `Set.from_list(...)`, got {:?}",
            value.kind,
        );
    };
    assert_eq!(
        method, "from_list",
        "synthesized method must be `from_list`"
    );
    assert_eq!(args.len(), 1, "from_list takes a single list argument");
    let ExprKind::Ident { name, .. } = &receiver.kind else {
        panic!(
            "expected receiver to be `Ident(\"Set\")`, got {:?}",
            receiver.kind
        );
    };
    assert_eq!(name, "Set", "synthesized receiver must be `Set`");
    let inner = &args[0].value;
    assert!(
        matches!(inner.kind, ExprKind::List { .. }),
        "synthesized arg must keep its `ExprKind::List` shape"
    );
    assert_eq!(
        inner.resolution,
        list_named_type(&checked, "Int"),
        "inner literal stamps `List<Int>` even though the outer expression is `Set<Int>`"
    );
}

#[test]
fn empty_set_literal_pins_element_from_binding_annotation() {
    let source = "
        numbers: Set<Int> = []
        numbers
        ";
    let checked = typecheck(&dedent(source));
    let body = body_statements(&checked);
    let assignment = body.first().expect("missing assignment");
    let Statement::Assignment { value, .. } = assignment else {
        panic!("expected Statement::Assignment, got {assignment:?}");
    };
    assert_eq!(value.resolution, set_named_type(&checked, "Int"));
    let ExprKind::MethodCall { method, args, .. } = &value.kind else {
        panic!(
            "expected empty `[]` to rewrite as `Set.from_list([])`, got {:?}",
            value.kind,
        );
    };
    assert_eq!(method, "from_list");
    let inner = &args[0].value;
    let ExprKind::List { elements } = &inner.kind else {
        panic!("expected ExprKind::List on the synthesized arg");
    };
    assert!(elements.is_empty(), "empty `[]` carries no elements");
    assert_eq!(inner.resolution, list_named_type(&checked, "Int"));
}
