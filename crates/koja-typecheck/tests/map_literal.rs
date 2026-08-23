//! Coverage for map-literal resolution. `["k": v, ...]` keeps its
//! `ExprKind::Map` shape on the sealed AST with `expr.resolution =
//! Map<K, V>` for the inferred entry types. The desugar to
//! `Map.new().put(k1, v1).put(k2, v2)` happens at IR-lower time.
//! These tests confirm the literal's shape, the per-entry
//! resolutions, and the bidirectional `Map<K, V>` hint flow.

use koja_ast::ast::{Expr, ExprKind, Literal, Statement};
use koja_ast::identifier::ResolvedType;
use koja_ast::util::dedent;
use koja_typecheck::CheckedProgram;

mod common;

use common::{
    global_leaf, global_named, script_body, trailing_expr, typecheck_script as typecheck,
};

fn map_named_type(checked: &CheckedProgram, key: &str, value: &str) -> ResolvedType {
    global_named(
        checked,
        "Map",
        vec![global_leaf(checked, key), global_leaf(checked, value)],
    )
}

fn assert_map_literal(expr: &Expr) -> &[(Expr, Expr)] {
    let ExprKind::Map { entries } = &expr.kind else {
        panic!("expected ExprKind::Map, got {:?}", expr.kind);
    };
    entries.as_slice()
}

#[test]
fn empty_map_literal_pins_entry_types_from_binding_annotation() {
    let source = "
        my_map: Map<String, Int> = [:]
        my_map
        ";
    let checked = typecheck(&dedent(source));
    let body = script_body(&checked);
    let assignment = body.first().expect("missing assignment");
    let Statement::Assignment { value, .. } = assignment else {
        panic!("expected Statement::Assignment, got {assignment:?}");
    };
    let entries = assert_map_literal(value);
    assert!(entries.is_empty(), "empty `[:]` carries no entries");
    assert_eq!(value.resolution, map_named_type(&checked, "String", "Int"));
}

#[test]
fn nonempty_map_literal_resolves_each_entry_in_source_order() {
    let source = "
        [\"a\": 10, \"b\": 20]
        ";
    let checked = typecheck(&dedent(source));
    let trailing = trailing_expr(&checked);
    let entries = assert_map_literal(trailing);
    assert_eq!(entries.len(), 2, "literal preserves both entries");
    for (key, _) in entries {
        assert!(
            matches!(&key.kind, ExprKind::String { .. }),
            "expected String literal key, got {:?}",
            key.kind,
        );
    }
    let values: Vec<String> = entries
        .iter()
        .map(|(_, value)| match &value.kind {
            ExprKind::Literal {
                value: Literal::Int(digits),
            } => digits.clone(),
            other => panic!("expected Int literal value, got {other:?}"),
        })
        .collect();
    assert_eq!(values, vec!["10", "20"]);
    assert_eq!(
        trailing.resolution,
        map_named_type(&checked, "String", "Int")
    );
}

#[test]
fn map_literal_typed_binding_pins_entry_types() {
    let source = "
        counts: Map<String, Int> = [\"hits\": 1, \"misses\": 2]
        counts
        ";
    let checked = typecheck(&dedent(source));
    let body = script_body(&checked);
    let assignment = body.first().expect("missing assignment");
    let Statement::Assignment { value, .. } = assignment else {
        panic!("expected Statement::Assignment, got {assignment:?}");
    };
    let entries = assert_map_literal(value);
    assert_eq!(entries.len(), 2);
    assert_eq!(value.resolution, map_named_type(&checked, "String", "Int"));
}

#[test]
fn non_generic_carrier_receives_ordered_entry_list() {
    let source = "
        struct Entries
          values: List<(String, Int)>
        end

        impl MapLiteral<String, Int> for Entries
          fn from_entries(entries: List<(String, Int)>) -> Self
            Entries{values: entries}
          end
        end

        entries: Entries = [\"a\": 1, \"a\": 2]
        entries
        ";
    let checked = typecheck(&dedent(source));
    let assignment = script_body(&checked).first().expect("missing assignment");
    let Statement::Assignment { value, .. } = assignment else {
        panic!("expected Statement::Assignment, got {assignment:?}");
    };
    let ExprKind::MethodCall { method, args, .. } = &value.kind else {
        panic!("expected MapLiteral carrier call, got {:?}", value.kind);
    };
    assert_eq!(method, "from_entries");
    let ExprKind::List { elements } = &args[0].value.kind else {
        panic!(
            "expected an ordered entry list, got {:?}",
            args[0].value.kind
        );
    };
    assert_eq!(elements.len(), 2);
    assert!(
        elements
            .iter()
            .all(|entry| matches!(entry.kind, ExprKind::Tuple { .. })),
        "each source entry must remain an ordered tuple",
    );
}
