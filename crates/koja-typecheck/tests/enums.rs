//! Typecheck coverage for the enum slice: declaration
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

use koja_ast::ast::{EnumConstructionData, ExprKind};
use koja_ast::identifier::{Resolution, ResolvedType};
use koja_ast::util::dedent;
use koja_typecheck::{EnumDefinition, ResolvedEnumVariant, ResolvedVariantData};

mod common;

use common::{
    PACKAGE, assert_script_fails_with, enum_definition, global_leaf, method_signature,
    package_leaf, registry_id, trailing_expr, typecheck_script as typecheck,
};

fn variant<'a>(definition: &'a EnumDefinition, name: &str) -> &'a ResolvedEnumVariant {
    definition
        .variants
        .iter()
        .find(|v| v.name == name)
        .unwrap_or_else(|| panic!("variant `{name}` missing from definition"))
}

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

#[test]
fn unit_variant_construction_resolves_to_enum_leaf() {
    let source = "
        enum Color
          Red
          Blue
        end

          Color.Red
        ";

    let checked = typecheck(&dedent(source));
    let trailing = trailing_expr(&checked);
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

          Color.Red(42)
        ";

    assert_script_fails_with(source, &["`TestApp.Color.Red`", "unit variant"]);
}

#[test]
fn tuple_variant_construction_resolves_argument_types() {
    let source = "
        enum Result
          Ok(Int)
          Err(String)
        end

          Result.Ok(42)
        ";

    let checked = typecheck(&dedent(source));
    let trailing = trailing_expr(&checked);
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

          Result.Ok(1, 2)
        ";

    assert_script_fails_with(
        source,
        &[
            "`TestApp.Result.Ok`",
            "expects 1 positional argument",
            "got 2",
        ],
    );
}

#[test]
fn tuple_variant_argument_type_mismatch_diagnoses() {
    let source = "
        enum Result
          Ok(Int)
        end

          Result.Ok(true)
        ";

    assert_script_fails_with(
        source,
        &[
            "argument 1 of `TestApp.Result.Ok`",
            "expects `Int`",
            "got `Bool`",
        ],
    );
}

#[test]
fn struct_variant_construction_resolves_field_types() {
    let source = "
        enum Shape
          Rect{w: Int, h: Int}
        end

          Shape.Rect{w: 10, h: 20}
        ";

    let checked = typecheck(&dedent(source));
    let trailing = trailing_expr(&checked);
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

          Shape.Rect{w: 10, h: 20, z: 30}
        ";

    assert_script_fails_with(source, &["`TestApp.Shape.Rect`", "no field `z`"]);
}

#[test]
fn struct_variant_missing_field_diagnoses() {
    let source = "
        enum Shape
          Rect{w: Int, h: Int}
        end

          Shape.Rect{w: 10}
        ";

    assert_script_fails_with(source, &["missing field `h`", "`TestApp.Shape.Rect`"]);
}

#[test]
fn struct_variant_wrong_field_type_diagnoses() {
    let source = "
        enum Shape
          Rect{w: Int, h: Int}
        end

          Shape.Rect{w: true, h: 20}
        ";

    assert_script_fails_with(
        source,
        &[
            "field `w`",
            "`TestApp.Shape.Rect`",
            "expects `Int`",
            "got `Bool`",
        ],
    );
}

#[test]
fn shape_mismatch_struct_supplied_to_tuple_variant_diagnoses() {
    let source = "
        enum Result
          Ok(Int)
        end

          Result.Ok{value: 1}
        ";

    assert_script_fails_with(
        source,
        &[
            "`TestApp.Result.Ok`",
            "tuple variant",
            "constructed with named fields",
        ],
    );
}

#[test]
fn unknown_enum_in_construction_diagnoses() {
    let source = "
          Missing.Variant
        ";

    assert_script_fails_with(source, &["does not recognize the enum type `Missing`"]);
}

#[test]
fn unknown_variant_in_construction_diagnoses() {
    let source = "
        enum Color
          Red
        end

          Color.Purple
        ";

    assert_script_fails_with(source, &["`TestApp.Color`", "no variant `Purple`"]);
}

#[test]
fn annotated_enum_diagnoses_feature_gap() {
    let source = "
        @derive
        enum Color
          Red
        end
        ";

    assert_script_fails_with(source, &["annotations on enum items"]);
}

#[test]
fn default_field_on_struct_variant_diagnoses_feature_gap() {
    let source = "
        enum Shape
          Rect{w: Int = 1, h: Int}
        end
        ";

    assert_script_fails_with(source, &["default field values"]);
}

// `Tag()` and `Tag {}` (empty tuple/struct variants) are rejected
// by the parser before typecheck sees them: `parse_type_expr` at
// the empty `()` / `parse_struct_field` at the empty `{}` produce
// parse-side diagnostics that surface from `parse_program`, so
// the typecheck-layer defense-in-depth check in
// [`crate::pipeline::lift_signatures::enums`] never fires from a
// surface program. Coverage for that fallback would need an AST
// constructed in-test (out of scope for this slice).

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

        extend Color
          fn primary -> Color
            Color.Red
          end
        end

          Color.primary()
        ";

    let checked = typecheck(&dedent(source));
    let signature = method_signature(&checked, "Color", "primary");
    assert_eq!(signature.return_type, package_leaf(&checked, "Color"));

    let trailing = trailing_expr(&checked);
    assert_eq!(trailing.resolution, package_leaf(&checked, "Color"));
}

#[test]
fn generic_enum_lifts_with_type_params_and_typeparam_payload_resolutions() {
    let source = "
        enum Result<T, E>
          Ok(T)
          Err(E)
        end
        ";

    let checked = typecheck(&dedent(source));
    let result_id = registry_id(&checked, PACKAGE, &["Result"]);
    let entry = checked
        .registry
        .get(result_id)
        .expect("registered Result entry");
    assert_eq!(entry.type_params, vec!["T".to_string(), "E".to_string()]);
    let definition = enum_definition(&checked, "Result");

    let ok = variant(definition, "Ok");
    let ResolvedVariantData::Tuple(ok_payload) = &ok.data else {
        panic!("expected Ok to be a tuple variant, got {:?}", ok.data);
    };
    assert!(matches!(
        &ok_payload[0],
        ResolvedType::Named {
            resolution: Resolution::TypeParam { owner, .. },
            ..
        } if *owner == result_id,
    ));
}

#[test]
fn generic_enum_tuple_variant_construction_infers_type_args() {
    let source = "
        enum Box<T>
          Of(T)
        end

          Box.Of(42)
        ";

    let checked = typecheck(&dedent(source));
    let box_id = registry_id(&checked, PACKAGE, &["Box"]);
    let int = global_leaf(&checked, "Int");
    let trailing = trailing_expr(&checked);
    assert_eq!(
        trailing.resolution,
        ResolvedType::Named {
            resolution: Resolution::Global(box_id),
            type_args: vec![int],
        },
    );
}

#[test]
fn generic_enum_partial_construction_diagnoses_phantom_for_unbound_param() {
    let source = "
        enum Result<T, E>
          Ok(T)
          Err(E)
        end

          Result.Ok(42)
        ";

    assert_script_fails_with(source, &["cannot infer type parameter `E`"]);
}

#[test]
fn generic_enum_unit_variant_construction_diagnoses_phantom() {
    // `Option<T>` is reserved as a stdlib stub (the iteration
    // protocol's canonical return type), so this fixture uses
    // `Maybe<T>` to test the same generic-unit-variant inference
    // shape without colliding with `Global.Option`.
    let source = "
        enum Maybe<T>
          Some(T)
          None
        end

          Maybe.None
        ";

    assert_script_fails_with(
        source,
        &[
            "cannot infer type parameter `T` of `TestApp.Maybe`",
            "unit variant `None`",
        ],
    );
}

#[test]
fn generic_enum_tuple_variant_arity_mismatch_diagnoses() {
    let source = "
        enum Box<T>
          Of(T, T)
        end

          Box.Of(1)
        ";

    assert_script_fails_with(source, &["expects 2 positional argument", "got 1"]);
}

#[test]
fn generic_enum_struct_variant_construction_infers_type_args() {
    let source = "
        enum Pair<T, U>
          Of { a: T, b: U }
        end

          Pair.Of{a: 1, b: \"x\"}
        ";

    let checked = typecheck(&dedent(source));
    let pair_id = registry_id(&checked, PACKAGE, &["Pair"]);
    let int = global_leaf(&checked, "Int");
    let string = global_leaf(&checked, "String");
    let trailing = trailing_expr(&checked);
    assert_eq!(
        trailing.resolution,
        ResolvedType::Named {
            resolution: Resolution::Global(pair_id),
            type_args: vec![int, string],
        },
    );
}
