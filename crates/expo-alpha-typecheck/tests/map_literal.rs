//! Coverage for map-literal resolution. `["k": v, ...]` keeps its
//! `ExprKind::Map` shape on the sealed AST with `expr.resolution =
//! Map<K, V>` for the inferred entry types; the desugar to
//! `Map.new().put(k1, v1).put(k2, v2)` happens at IR-lower time.
//! These tests confirm the literal's shape, the per-entry
//! resolutions, and the bidirectional `Map<K, V>` hint flow.

use expo_alpha_typecheck::CheckedProgram;
use expo_ast::ast::{Expr, ExprKind, Item, Literal, Statement};
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

fn map_named_type(checked: &CheckedProgram, key: &str, value: &str) -> ResolvedType {
    let map_ident = Identifier::new("Global", vec!["Map".to_string()]);
    let (map_id, _) = checked
        .registry
        .lookup(&map_ident)
        .unwrap_or_else(|| panic!("autoimported `Global.Map` missing from registry"));
    let key_ident = Identifier::new("Global", vec![key.to_string()]);
    let (key_id, _) = checked
        .registry
        .lookup(&key_ident)
        .unwrap_or_else(|| panic!("stdlib stub `Global.{key}` missing from registry"));
    let value_ident = Identifier::new("Global", vec![value.to_string()]);
    let (value_id, _) = checked
        .registry
        .lookup(&value_ident)
        .unwrap_or_else(|| panic!("stdlib stub `Global.{value}` missing from registry"));
    ResolvedType::Named {
        resolution: Resolution::Global(map_id),
        type_args: vec![
            ResolvedType::leaf(Resolution::Global(key_id)),
            ResolvedType::leaf(Resolution::Global(value_id)),
        ],
    }
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
        fn main
          my_map: Map<String, Int> = [:]
          my_map
        end
        ";
    let checked = typecheck(&dedent(source));
    let body = main_body(&checked);
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
        fn main
          [\"a\": 10, \"b\": 20]
        end
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
        fn main
          counts: Map<String, Int> = [\"hits\": 1, \"misses\": 2]
          counts
        end
        ";
    let checked = typecheck(&dedent(source));
    let body = main_body(&checked);
    let assignment = body.first().expect("missing assignment");
    let Statement::Assignment { value, .. } = assignment else {
        panic!("expected Statement::Assignment, got {assignment:?}");
    };
    let entries = assert_map_literal(value);
    assert_eq!(entries.len(), 2);
    assert_eq!(value.resolution, map_named_type(&checked, "String", "Int"));
}
