//! Typecheck coverage for the alpha enum slice: declaration
//! registration + lift, the three variant shapes (Unit, Tuple,
//! Struct) at construction time, static method dispatch through an
//! enum receiver, plus the per-feature gap diagnostics (generics,
//! annotations, default field values on struct variants, empty
//! Tuple `Foo()` / Struct `Foo {}` payloads, impl block on an enum
//! is admitted) and per-construction-site validation diagnostics
//! (unknown enum, unknown variant, shape mismatch, arity mismatch
//! on tuple variants, unknown / missing / wrong-typed field on
//! struct variants).
//!
//! Mirrors `tests/structs.rs` shape for shared concerns (lift /
//! construction / static methods) so the diagnostic surface stays
//! parallel between the two type-decl families.

use expo_alpha_typecheck::{
    CheckedProgram, EnumDefinition, FunctionSignature, GlobalKind, ResolvedEnumVariant,
    ResolvedVariantData,
};
use expo_ast::ast::{EnumConstructionData, Expr, ExprKind, Item, Statement};
use expo_ast::identifier::{Identifier, Resolution, ResolvedType};
use expo_ast::util::dedent;

mod common;

use common::{
    PACKAGE, diagnostic_messages, typecheck_file as typecheck,
    typecheck_file_fail as typecheck_fail,
};

fn enum_definition<'a>(checked: &'a CheckedProgram, name: &str) -> &'a EnumDefinition {
    let ident = Identifier::new(PACKAGE, vec![name.to_string()]);
    let (_, entry) = checked
        .registry
        .lookup(&ident)
        .unwrap_or_else(|| panic!("`{ident}` not found in registry"));
    match &entry.kind {
        GlobalKind::Enum(Some(definition)) => definition,
        other => panic!("expected lifted Enum(Some(_)) for `{ident}`, got {other:?}"),
    }
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

fn variant<'a>(definition: &'a EnumDefinition, name: &str) -> &'a ResolvedEnumVariant {
    definition
        .variants
        .iter()
        .find(|v| v.name == name)
        .unwrap_or_else(|| panic!("variant `{name}` missing from definition"))
}

// ---------------------------------------------------------------------------
// Decl registration / lift
// ---------------------------------------------------------------------------

#[test]
fn unit_only_enum_lifts_with_unit_variants_in_declaration_order() {
    let source = "
        enum Color
          Red
          Green
          Blue
        end
        ";

    let checked = typecheck(&dedent(source));
    let definition = enum_definition(&checked, "Color");
    let names: Vec<&str> = definition
        .variants
        .iter()
        .map(|v| v.name.as_str())
        .collect();
    assert_eq!(names, vec!["Red", "Green", "Blue"]);
    for v in &definition.variants {
        assert!(
            matches!(v.data, ResolvedVariantData::Unit),
            "expected Unit shape for `{}`, got {:?}",
            v.name,
            v.data,
        );
    }
}

#[test]
fn mixed_shape_enum_lifts_each_variant_with_its_payload() {
    let source = "
        enum Shape
          Circle(Int)
          Rect{w: Int, h: Int}
          Empty
        end
        ";

    let checked = typecheck(&dedent(source));
    let definition = enum_definition(&checked, "Shape");
    let int = global_leaf(&checked, "Int");

    let circle = variant(definition, "Circle");
    match &circle.data {
        ResolvedVariantData::Tuple(types) => {
            assert_eq!(types, &vec![int.clone()]);
        }
        other => panic!("expected Tuple shape for Circle, got {other:?}"),
    }

    let rect = variant(definition, "Rect");
    match &rect.data {
        ResolvedVariantData::Struct(fields) => {
            let names: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
            assert_eq!(names, vec!["w", "h"]);
            for field in fields {
                assert_eq!(field.ty, int);
            }
        }
        other => panic!("expected Struct shape for Rect, got {other:?}"),
    }

    let empty = variant(definition, "Empty");
    assert!(matches!(empty.data, ResolvedVariantData::Unit));
}

#[test]
fn tuple_variant_with_user_struct_payload_resolves_through_registry() {
    let source = "
        struct Inner
          n: Int
        end

        enum Wrap
          Some(Inner)
          None
        end
        ";

    let checked = typecheck(&dedent(source));
    let definition = enum_definition(&checked, "Wrap");
    let inner = package_leaf(&checked, "Inner");
    let some = variant(definition, "Some");
    match &some.data {
        ResolvedVariantData::Tuple(types) => {
            assert_eq!(types, &vec![inner]);
        }
        other => panic!("expected Tuple(Inner), got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Construction — Unit
// ---------------------------------------------------------------------------

#[test]
fn unit_variant_construction_resolves_to_enum_leaf() {
    let source = "
        enum Color
          Red
          Blue
        end

        fn main
          Color.Red
        end
        ";

    let checked = typecheck(&dedent(source));
    let trailing = body_trailing_expr(&checked, "main");
    assert_eq!(trailing.resolution, package_leaf(&checked, "Color"));

    let ExprKind::EnumConstruction {
        type_path,
        variant,
        data,
    } = &trailing.kind
    else {
        panic!("expected EnumConstruction, got {:?}", trailing.kind);
    };
    assert_eq!(type_path, &["Color".to_string()]);
    assert_eq!(variant, "Red");
    assert!(matches!(data, EnumConstructionData::Unit));
}

#[test]
fn unit_variant_with_payload_supplied_diagnoses_shape_mismatch() {
    let source = "
        enum Color
          Red
        end

        fn main
          Color.Red(42)
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("`TestApp.Color.Red`") && m.contains("unit variant")),
        "expected unit-shape mismatch diagnostic, got {messages:?}",
    );
}

// ---------------------------------------------------------------------------
// Construction — Tuple
// ---------------------------------------------------------------------------

#[test]
fn tuple_variant_construction_resolves_argument_types() {
    let source = "
        enum Result
          Ok(Int)
          Err(String)
        end

        fn main
          Result.Ok(42)
        end
        ";

    let checked = typecheck(&dedent(source));
    let trailing = body_trailing_expr(&checked, "main");
    assert_eq!(trailing.resolution, package_leaf(&checked, "Result"));

    let ExprKind::EnumConstruction { variant, data, .. } = &trailing.kind else {
        panic!("expected EnumConstruction, got {:?}", trailing.kind);
    };
    assert_eq!(variant, "Ok");
    let EnumConstructionData::Tuple(args) = data else {
        panic!("expected Tuple data, got {data:?}");
    };
    assert_eq!(args.len(), 1);
    assert_eq!(args[0].resolution, global_leaf(&checked, "Int"));
}

#[test]
fn tuple_variant_arity_mismatch_diagnoses() {
    let source = "
        enum Result
          Ok(Int)
        end

        fn main
          Result.Ok(1, 2)
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages.iter().any(|m| m.contains("`TestApp.Result.Ok`")
            && m.contains("expects 1 positional argument")
            && m.contains("got 2")),
        "expected tuple-arity diagnostic, got {messages:?}",
    );
}

#[test]
fn tuple_variant_argument_type_mismatch_diagnoses() {
    let source = "
        enum Result
          Ok(Int)
        end

        fn main
          Result.Ok(true)
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("argument 1 of `TestApp.Result.Ok`")
                && m.contains("expects `Int`")
                && m.contains("got `Bool`")),
        "expected tuple-arg-type diagnostic, got {messages:?}",
    );
}

// ---------------------------------------------------------------------------
// Construction — Struct
// ---------------------------------------------------------------------------

#[test]
fn struct_variant_construction_resolves_field_types() {
    let source = "
        enum Shape
          Rect{w: Int, h: Int}
        end

        fn main
          Shape.Rect{w: 10, h: 20}
        end
        ";

    let checked = typecheck(&dedent(source));
    let trailing = body_trailing_expr(&checked, "main");
    assert_eq!(trailing.resolution, package_leaf(&checked, "Shape"));

    let ExprKind::EnumConstruction { data, .. } = &trailing.kind else {
        panic!("expected EnumConstruction, got {:?}", trailing.kind);
    };
    let EnumConstructionData::Struct(fields) = data else {
        panic!("expected Struct data, got {data:?}");
    };
    let int = global_leaf(&checked, "Int");
    for field in fields {
        assert_eq!(field.value.resolution, int);
    }
}

#[test]
fn struct_variant_unknown_field_diagnoses() {
    let source = "
        enum Shape
          Rect{w: Int, h: Int}
        end

        fn main
          Shape.Rect{w: 10, h: 20, z: 30}
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("`TestApp.Shape.Rect`") && m.contains("no field `z`")),
        "expected unknown-field diagnostic, got {messages:?}",
    );
}

#[test]
fn struct_variant_missing_field_diagnoses() {
    let source = "
        enum Shape
          Rect{w: Int, h: Int}
        end

        fn main
          Shape.Rect{w: 10}
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("missing field `h`") && m.contains("`TestApp.Shape.Rect`")),
        "expected missing-field diagnostic, got {messages:?}",
    );
}

#[test]
fn struct_variant_wrong_field_type_diagnoses() {
    let source = "
        enum Shape
          Rect{w: Int, h: Int}
        end

        fn main
          Shape.Rect{w: true, h: 20}
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages.iter().any(|m| m.contains("field `w`")
            && m.contains("`TestApp.Shape.Rect`")
            && m.contains("expects `Int`")
            && m.contains("got `Bool`")),
        "expected wrong-type diagnostic, got {messages:?}",
    );
}

#[test]
fn shape_mismatch_struct_supplied_to_tuple_variant_diagnoses() {
    let source = "
        enum Result
          Ok(Int)
        end

        fn main
          Result.Ok{value: 1}
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages.iter().any(|m| m.contains("`TestApp.Result.Ok`")
            && m.contains("tuple variant")
            && m.contains("constructed with named fields")),
        "expected shape-mismatch diagnostic, got {messages:?}",
    );
}

// ---------------------------------------------------------------------------
// Negative — unknown enum / variant
// ---------------------------------------------------------------------------

#[test]
fn unknown_enum_in_construction_diagnoses() {
    let source = "
        fn main
          Missing.Variant
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("does not recognize the enum type `Missing`")),
        "expected unknown-enum diagnostic, got {messages:?}",
    );
}

#[test]
fn unknown_variant_in_construction_diagnoses() {
    let source = "
        enum Color
          Red
        end

        fn main
          Color.Purple
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("`TestApp.Color`") && m.contains("no variant `Purple`")),
        "expected unknown-variant diagnostic, got {messages:?}",
    );
}

// ---------------------------------------------------------------------------
// Negative — feature gaps
// ---------------------------------------------------------------------------

#[test]
fn generic_enum_diagnoses_feature_gap() {
    let source = "
        enum Wrapper<T>
          Some(Int)
          None
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("does not yet support generic enums")),
        "expected generic-enum gap diagnostic, got {messages:?}",
    );
}

#[test]
fn annotated_enum_diagnoses_feature_gap() {
    let source = "
        @derive
        enum Color
          Red
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("annotations on enum items")),
        "expected enum-annotation gap diagnostic, got {messages:?}",
    );
}

#[test]
fn default_field_on_struct_variant_diagnoses_feature_gap() {
    let source = "
        enum Shape
          Rect{w: Int = 1, h: Int}
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages.iter().any(|m| m.contains("default field values")),
        "expected default-field-value gap diagnostic, got {messages:?}",
    );
}

// `Tag()` and `Tag {}` (empty tuple/struct variants) are rejected
// by the parser before typecheck sees them — `parse_type_expr` at
// the empty `()` / `parse_struct_field` at the empty `{}` produce
// parse-side diagnostics that surface from `parse_program`, so
// the typecheck-layer defense-in-depth check in
// [`crate::pipeline::lift_signatures::enums`] never fires from a
// surface program. Coverage for that fallback would need an AST
// constructed in-test (out of scope for this slice).

// ---------------------------------------------------------------------------
// Static methods on enum receivers
// ---------------------------------------------------------------------------

fn method_signature<'a>(
    checked: &'a CheckedProgram,
    type_name: &str,
    method_name: &str,
) -> &'a FunctionSignature {
    let identifier = Identifier::new(
        PACKAGE,
        vec![type_name.to_string(), method_name.to_string()],
    );
    let (_, entry) = checked
        .registry
        .lookup(&identifier)
        .unwrap_or_else(|| panic!("`{identifier}` not registered"));
    match &entry.kind {
        GlobalKind::Function(Some(signature)) => signature,
        other => panic!("expected lifted Function for `{identifier}`, got {other:?}"),
    }
}

#[test]
fn inline_static_method_on_enum_registers_under_qualified_identifier() {
    let source = "
        enum Color
          Red
          Blue

          fn primary -> Color
            Color.Red
          end
        end
        ";

    let checked = typecheck(&dedent(source));
    let signature = method_signature(&checked, "Color", "primary");
    assert!(signature.params.is_empty());
    assert_eq!(signature.return_type, package_leaf(&checked, "Color"));
}

#[test]
fn impl_block_on_enum_admits_static_methods() {
    let source = "
        enum Color
          Red
          Blue
        end

        impl Color
          fn primary -> Color
            Color.Red
          end
        end

        fn main
          Color.primary()
        end
        ";

    let checked = typecheck(&dedent(source));
    let signature = method_signature(&checked, "Color", "primary");
    assert_eq!(signature.return_type, package_leaf(&checked, "Color"));

    let trailing = body_trailing_expr(&checked, "main");
    assert_eq!(trailing.resolution, package_leaf(&checked, "Color"));
}
