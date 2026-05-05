//! Typecheck coverage for the alpha struct slice: declaration
//! registration, signature lifting, struct-literal construction, and
//! field-access resolution. Includes the per-feature gap diagnostics
//! (generics, methods, annotations, default field values) plus the
//! per-construction-site validation diagnostics (unknown / extra /
//! missing / duplicate / wrong-typed field, non-struct receiver).

use expo_alpha_typecheck::{CheckedProgram, GlobalKind, ResolvedStructField, StructDefinition};
use expo_ast::ast::{Expr, ExprKind, Item, Statement, StructDecl};
use expo_ast::identifier::{Identifier, Resolution, ResolvedType};
use expo_ast::util::dedent;

mod common;

use common::{
    PACKAGE, diagnostic_messages, typecheck_file as typecheck,
    typecheck_file_fail as typecheck_fail,
};

fn struct_definition<'a>(checked: &'a CheckedProgram, name: &str) -> &'a StructDefinition {
    let ident = Identifier::new(PACKAGE, vec![name.to_string()]);
    let (_, entry) = checked
        .registry
        .lookup(&ident)
        .unwrap_or_else(|| panic!("`{ident}` not found in registry"));
    match &entry.kind {
        GlobalKind::Struct(Some(definition)) => definition,
        other => panic!("expected lifted Struct(Some(_)) for `{ident}`, got {other:?}"),
    }
}

fn find_struct_decl<'a>(checked: &'a CheckedProgram, name: &str) -> &'a StructDecl {
    let pkg = checked
        .packages
        .iter()
        .find(|p| p.package == PACKAGE)
        .expect("checked program is missing the test package");
    for file in &pkg.files {
        for item in &file.items {
            if let Item::Struct(decl) = item
                && decl.name == name
            {
                return decl;
            }
        }
    }
    panic!("struct `{name}` not found in checked program");
}

fn body_trailing_expr<'a>(checked: &'a CheckedProgram, fn_name: &str) -> &'a Expr {
    let pkg = checked
        .packages
        .iter()
        .find(|p| p.package == PACKAGE)
        .expect("checked program is missing the test package");
    for file in &pkg.files {
        for item in &file.items {
            if let Item::Function(function) = item
                && function.name == fn_name
            {
                let body = function.body.as_deref().expect("function has no body");
                return match body.last().expect("function body is empty") {
                    Statement::Expr(expr) => expr,
                    other => panic!("expected trailing Statement::Expr, got {other:?}"),
                };
            }
        }
    }
    panic!("fn `{fn_name}` not found in checked program");
}

fn global_leaf(checked: &CheckedProgram, name: &str) -> ResolvedType {
    let ident = Identifier::new("Global", vec![name.to_string()]);
    let (id, _) = checked
        .registry
        .lookup(&ident)
        .unwrap_or_else(|| panic!("stdlib stub `Global.{name}` missing from registry"));
    ResolvedType::leaf(Resolution::Global(id))
}

fn package_leaf(checked: &CheckedProgram, name: &str) -> ResolvedType {
    let ident = Identifier::new(PACKAGE, vec![name.to_string()]);
    let (id, _) = checked
        .registry
        .lookup(&ident)
        .unwrap_or_else(|| panic!("`{ident}` missing from registry"));
    ResolvedType::leaf(Resolution::Global(id))
}

// ---------------------------------------------------------------------------
// Decl registration / lift
// ---------------------------------------------------------------------------

#[test]
fn struct_decl_registers_with_lifted_definition() {
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
    let definition = struct_definition(&checked, "Point");
    let int = global_leaf(&checked, "Int");

    assert_eq!(definition.fields.len(), 2);
    assert_eq!(definition.fields[0].name, "x");
    assert_eq!(definition.fields[0].ty, int);
    assert_eq!(definition.fields[1].name, "y");
    assert_eq!(definition.fields[1].ty, int);

    let decl = find_struct_decl(&checked, "Point");
    assert_eq!(decl.name, "Point");
    assert_eq!(decl.fields.len(), 2);
}

#[test]
fn empty_struct_lifts_with_zero_fields() {
    let source = "
        struct Marker
        end

        fn main
          1
        end
        ";

    let checked = typecheck(&dedent(source));
    let definition = struct_definition(&checked, "Marker");
    assert!(
        definition.fields.is_empty(),
        "empty struct should have zero fields, got {} field(s)",
        definition.fields.len(),
    );
}

#[test]
fn struct_with_mixed_field_types_lifts_each_field_independently() {
    let source = "
        struct Mixed
          flag: Bool
          name: String
          count: Int
        end

        fn main
          1
        end
        ";

    let checked = typecheck(&dedent(source));
    let definition = struct_definition(&checked, "Mixed");
    let bool_ty = global_leaf(&checked, "Bool");
    let string_ty = global_leaf(&checked, "String");
    let int_ty = global_leaf(&checked, "Int");

    let names: Vec<&str> = definition.fields.iter().map(field_name).collect();
    assert_eq!(names, vec!["flag", "name", "count"]);

    let types: Vec<&ResolvedType> = definition.fields.iter().map(|f| &f.ty).collect();
    assert_eq!(types, vec![&bool_ty, &string_ty, &int_ty]);
}

#[test]
fn nested_struct_field_resolves_to_inner_struct_id() {
    let source = "
        struct Inner
          n: Int
        end

        struct Outer
          inner: Inner
          tag: Bool
        end

        fn main
          1
        end
        ";

    let checked = typecheck(&dedent(source));
    let outer = struct_definition(&checked, "Outer");
    let inner_leaf = package_leaf(&checked, "Inner");
    let bool_ty = global_leaf(&checked, "Bool");

    assert_eq!(outer.fields.len(), 2);
    assert_eq!(outer.fields[0].ty, inner_leaf);
    assert_eq!(outer.fields[1].ty, bool_ty);
}

// ---------------------------------------------------------------------------
// Construction
// ---------------------------------------------------------------------------

#[test]
fn struct_construction_resolves_to_struct_leaf() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn main
          Point{x: 1, y: 2}
        end
        ";

    let checked = typecheck(&dedent(source));
    let trailing = body_trailing_expr(&checked, "main");
    let expected = package_leaf(&checked, "Point");
    assert_eq!(trailing.resolution, expected);

    let ExprKind::StructConstruction { type_path, fields } = &trailing.kind else {
        panic!("expected StructConstruction, got {:?}", trailing.kind);
    };
    assert_eq!(type_path, &["Point".to_string()]);
    assert_eq!(fields.len(), 2);
    let int = global_leaf(&checked, "Int");
    for field in fields {
        assert_eq!(field.value.resolution, int);
    }
}

#[test]
fn struct_construction_accepts_out_of_order_fields() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn main
          Point{y: 2, x: 1}
        end
        ";

    let checked = typecheck(&dedent(source));
    let trailing = body_trailing_expr(&checked, "main");
    let expected = package_leaf(&checked, "Point");
    assert_eq!(trailing.resolution, expected);
}

// ---------------------------------------------------------------------------
// Field access
// ---------------------------------------------------------------------------

#[test]
fn field_access_resolves_to_declared_field_type() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn main
          Point{x: 1, y: 2}.x
        end
        ";

    let checked = typecheck(&dedent(source));
    let trailing = body_trailing_expr(&checked, "main");
    let int = global_leaf(&checked, "Int");
    assert_eq!(trailing.resolution, int);

    let ExprKind::FieldAccess { receiver, field } = &trailing.kind else {
        panic!("expected FieldAccess, got {:?}", trailing.kind);
    };
    assert_eq!(field, "x");
    let point = package_leaf(&checked, "Point");
    assert_eq!(receiver.resolution, point);
}

#[test]
fn nested_field_access_resolves_through_inner_struct() {
    let source = "
        struct Inner
          n: Int
        end

        struct Outer
          inner: Inner
        end

        fn main
          Outer{inner: Inner{n: 5}}.inner.n
        end
        ";

    let checked = typecheck(&dedent(source));
    let trailing = body_trailing_expr(&checked, "main");
    let int = global_leaf(&checked, "Int");
    assert_eq!(trailing.resolution, int);
}

// ---------------------------------------------------------------------------
// Negative — feature gaps
// ---------------------------------------------------------------------------

#[test]
fn generic_struct_diagnoses_feature_gap() {
    let source = "
        struct Wrapper<T>
          value: Int
        end

        fn main
          1
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("does not yet support generic structs")),
        "expected generic-struct gap diagnostic, got {messages:?}",
    );
}

#[test]
fn struct_with_inline_function_diagnoses_feature_gap() {
    let source = "
        struct Point
          x: Int
          y: Int

          fn origin -> Int
            0
          end
        end

        fn main
          1
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("functions inside `struct ... end`")),
        "expected struct-fn gap diagnostic, got {messages:?}",
    );
}

#[test]
fn annotated_struct_diagnoses_feature_gap() {
    let source = "
        @derive
        struct Point
          x: Int
        end

        fn main
          1
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("annotations on struct items")),
        "expected struct-annotation gap diagnostic, got {messages:?}",
    );
}

#[test]
fn default_field_value_diagnoses_feature_gap() {
    let source = "
        struct Point
          x: Int = 0
          y: Int
        end

        fn main
          1
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages.iter().any(|m| m.contains("default field values")),
        "expected default-field-value gap diagnostic, got {messages:?}",
    );
}

// ---------------------------------------------------------------------------
// Negative — construction validation
// ---------------------------------------------------------------------------

#[test]
fn unknown_struct_type_diagnoses() {
    let source = "
        fn main
          Missing{x: 1}
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("does not recognize the struct type `Missing`")),
        "expected unknown-struct diagnostic, got {messages:?}",
    );
}

#[test]
fn primitive_struct_construction_diagnoses() {
    let source = "
        fn main
          Int{x: 1}
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("cannot construct primitive type `Global.Int`")),
        "expected primitive-construction diagnostic, got {messages:?}",
    );
}

#[test]
fn unknown_field_in_construction_diagnoses() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn main
          Point{x: 1, y: 2, z: 3}
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("`TestApp.Point` has no field `z`")),
        "expected unknown-field diagnostic, got {messages:?}",
    );
}

#[test]
fn missing_field_in_construction_diagnoses() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn main
          Point{x: 1}
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("missing field `y`") && m.contains("`TestApp.Point`")),
        "expected missing-field diagnostic, got {messages:?}",
    );
}

#[test]
fn duplicate_field_in_construction_diagnoses() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn main
          Point{x: 1, x: 2, y: 3}
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("field `x`") && m.contains("initialized twice")),
        "expected duplicate-field diagnostic, got {messages:?}",
    );
}

#[test]
fn wrong_field_type_in_construction_diagnoses() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn main
          Point{x: true, y: 2}
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages.iter().any(|m| m.contains("field `x`")
            && m.contains("expects `Int`")
            && m.contains("got `Bool`")),
        "expected wrong-field-type diagnostic, got {messages:?}",
    );
}

// ---------------------------------------------------------------------------
// Negative — field access validation
// ---------------------------------------------------------------------------

#[test]
fn unknown_field_on_struct_access_diagnoses() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn main
          Point{x: 1, y: 2}.z
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("`TestApp.Point` has no field `z`")),
        "expected unknown-field-access diagnostic, got {messages:?}",
    );
}

#[test]
fn field_access_on_non_struct_diagnoses() {
    let source = "
        fn main
          1.x
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("field access requires a struct receiver")),
        "expected non-struct-receiver diagnostic, got {messages:?}",
    );
}

fn field_name(field: &ResolvedStructField) -> &str {
    field.name.as_str()
}
