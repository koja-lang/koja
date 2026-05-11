//! Runtime coverage for the enum slice in
//! [`expo_alpha_ir_eval::Interpreter`]: `IRInstruction::EnumConstruct`
//! materializes a [`Value::Enum`] carrying the receiver's symbol,
//! the discriminant tag, the variant name (cached for `Display`),
//! and the per-shape [`EnumPayload`]:
//!
//! - **Unit** — `Color.Red` → `EnumPayload::Unit`
//! - **Tuple** — `Result.Ok(42)` → `EnumPayload::Tuple([Int(42)])`
//! - **Struct** — `Shape.Rect{w: 1, h: 2}` →
//!   `EnumPayload::Struct([("w", Int(1)), ("h", Int(2))])`
//!
//! Plus the `Display` rendering for each shape so the runtime
//! printer (when it gains an enum arm) sees a stable surface.

use expo_alpha_ir_eval::{EnumPayload, Value};
use expo_ast::util::dedent;

mod common;

fn evaluate_script(source: &str) -> Value {
    common::evaluate_script(source).expect("interpreter should not error on this fixture")
}

// ---------------------------------------------------------------------------
// Unit variants
// ---------------------------------------------------------------------------

#[test]
fn unit_variant_construction_yields_value_enum_with_unit_payload() {
    let source = "
        enum Color
          Red
          Blue
        end

        Color.Red
        ";

    let value = evaluate_script(&dedent(source));
    let Value::Enum {
        symbol,
        name,
        tag,
        payload,
    } = value
    else {
        panic!("expected Value::Enum, got {value:?}");
    };
    assert_eq!(symbol.mangled(), "TestApp.Color");
    assert_eq!(name, "Red");
    assert_eq!(tag.0, 0);
    assert_eq!(payload, EnumPayload::Unit);
}

#[test]
fn higher_position_unit_variant_carries_position_as_tag() {
    let source = "
        enum Color
          Red
          Green
          Blue
        end

        Color.Blue
        ";

    let value = evaluate_script(&dedent(source));
    let Value::Enum { name, tag, .. } = value else {
        panic!("expected Value::Enum, got {value:?}");
    };
    assert_eq!(name, "Blue");
    assert_eq!(tag.0, 2);
}

// ---------------------------------------------------------------------------
// Tuple variants
// ---------------------------------------------------------------------------

#[test]
fn tuple_variant_carries_evaluated_positional_payload() {
    let source = "
        enum Result
          Ok(Int)
          Err(String)
        end

        Result.Ok(42)
        ";

    let value = evaluate_script(&dedent(source));
    let Value::Enum {
        name, tag, payload, ..
    } = value
    else {
        panic!("expected Value::Enum, got {value:?}");
    };
    assert_eq!(name, "Ok");
    assert_eq!(tag.0, 0);
    assert_eq!(payload, EnumPayload::Tuple(vec![Value::Int(42)]));
}

#[test]
fn tuple_variant_with_string_payload_carries_string_value() {
    let source = "
        enum Result
          Ok(Int)
          Err(String)
        end

        Result.Err(\"boom\")
        ";

    let value = evaluate_script(&dedent(source));
    let Value::Enum { payload, tag, .. } = value else {
        panic!("expected Value::Enum, got {value:?}");
    };
    assert_eq!(tag.0, 1);
    assert_eq!(
        payload,
        EnumPayload::Tuple(vec![Value::String("boom".into())]),
    );
}

#[test]
fn tuple_variant_with_multiple_elements_carries_all_in_order() {
    let source = "
        enum Pair
          Of(Int, Bool)
        end

        Pair.Of(7, true)
        ";

    let value = evaluate_script(&dedent(source));
    let Value::Enum { payload, .. } = value else {
        panic!("expected Value::Enum, got {value:?}");
    };
    assert_eq!(
        payload,
        EnumPayload::Tuple(vec![Value::Int(7), Value::Bool(true)]),
    );
}

// ---------------------------------------------------------------------------
// Struct variants
// ---------------------------------------------------------------------------

#[test]
fn struct_variant_carries_named_fields_in_declaration_order() {
    let source = "
        enum Shape
          Rect{w: Int, h: Int}
        end

        Shape.Rect{w: 10, h: 20}
        ";

    let value = evaluate_script(&dedent(source));
    let Value::Enum { payload, .. } = value else {
        panic!("expected Value::Enum, got {value:?}");
    };
    assert_eq!(
        payload,
        EnumPayload::Struct(vec![
            ("w".to_string(), Value::Int(10)),
            ("h".to_string(), Value::Int(20)),
        ]),
    );
}

#[test]
fn struct_variant_canonicalizes_out_of_order_field_inits() {
    let source = "
        enum Shape
          Rect{w: Int, h: Int}
        end

        Shape.Rect{h: 20, w: 10}
        ";

    let value = evaluate_script(&dedent(source));
    let Value::Enum { payload, .. } = value else {
        panic!("expected Value::Enum, got {value:?}");
    };
    let EnumPayload::Struct(fields) = payload else {
        panic!("expected Struct payload");
    };
    assert_eq!(fields[0].0, "w");
    assert_eq!(fields[0].1, Value::Int(10));
    assert_eq!(fields[1].0, "h");
    assert_eq!(fields[1].1, Value::Int(20));
}

// ---------------------------------------------------------------------------
// Display rendering
// ---------------------------------------------------------------------------

#[test]
fn unit_variant_display_renders_qualified_name_only() {
    let source = "
        enum Color
          Red
        end

        Color.Red
        ";

    let value = evaluate_script(&dedent(source));
    assert_eq!(format!("{value}"), "TestApp.Color.Red");
}

#[test]
fn tuple_variant_display_renders_parenthesized_payload() {
    let source = "
        enum Pair
          Of(Int, Bool)
        end

        Pair.Of(7, true)
        ";

    let value = evaluate_script(&dedent(source));
    assert_eq!(format!("{value}"), "TestApp.Pair.Of(7, true)");
}

#[test]
fn struct_variant_display_renders_named_payload_in_braces() {
    let source = "
        enum Shape
          Rect{w: Int, h: Int}
        end

        Shape.Rect{w: 10, h: 20}
        ";

    let value = evaluate_script(&dedent(source));
    assert_eq!(format!("{value}"), "TestApp.Shape.Rect{w: 10, h: 20}");
}

// ---------------------------------------------------------------------------
// Static method dispatch via enum receiver
// ---------------------------------------------------------------------------

#[test]
fn static_method_on_enum_returns_constructed_variant_value() {
    let source = "
        enum Color
          Red
          Blue

          fn primary -> Color
            Color.Red
          end
        end

        Color.primary()
        ";

    let value = evaluate_script(&dedent(source));
    let Value::Enum { name, .. } = value else {
        panic!("expected Value::Enum, got {value:?}");
    };
    assert_eq!(name, "Red");
}
