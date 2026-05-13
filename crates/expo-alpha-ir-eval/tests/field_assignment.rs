//! Runtime coverage for multi-segment field assignment in the
//! interpreter: `p.x = v`, `a.b.c = v`, and `p.x += 1` round-trip
//! through the SSA-pure `FieldGet → FieldSet` rebuild and update the
//! head local's slot to the freshly-assembled struct value.

use expo_alpha_ir_eval::Value;
use expo_ast::util::dedent;

mod common;

fn evaluate_script(source: &str) -> Value {
    common::evaluate_script(source).expect("interpreter should not error on this fixture")
}

#[test]
fn depth_one_field_write_updates_local_slot() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        p = Point{x: 1, y: 2}
        p.x = 10
        p.x
        ";

    let value = evaluate_script(&dedent(source));
    assert_eq!(
        value,
        Value::Int(10),
        "expected `p.x` to read the just-written `10`",
    );
}

#[test]
fn depth_one_field_write_preserves_other_fields() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        p = Point{x: 1, y: 2}
        p.x = 10
        p.y
        ";

    let value = evaluate_script(&dedent(source));
    assert_eq!(
        value,
        Value::Int(2),
        "expected the SSA-pure rebuild to leave `p.y` untouched",
    );
}

#[test]
fn depth_two_field_write_updates_nested_struct() {
    let source = "
        struct Inner
          n: Int
        end

        struct Outer
          inner: Inner
          tag: Bool
        end

        o = Outer{inner: Inner{n: 1}, tag: true}
        o.inner.n = 42
        o.inner.n
        ";

    let value = evaluate_script(&dedent(source));
    assert_eq!(value, Value::Int(42));
}

#[test]
fn depth_two_field_write_preserves_unrelated_outer_fields() {
    let source = "
        struct Inner
          n: Int
        end

        struct Outer
          inner: Inner
          tag: Bool
        end

        o = Outer{inner: Inner{n: 1}, tag: true}
        o.inner.n = 42
        o.tag
        ";

    let value = evaluate_script(&dedent(source));
    assert_eq!(
        value,
        Value::Bool(true),
        "expected the depth-2 rebuild to leave `o.tag` untouched",
    );
}

#[test]
fn compound_assign_on_field_updates_through_field_get_and_field_set() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        p = Point{x: 7, y: 0}
        p.x += 3
        p.x
        ";

    let value = evaluate_script(&dedent(source));
    assert_eq!(value, Value::Int(10));
}
