//! Runtime coverage for package-level constants: primitives inline and
//! pooled compounds materialize through [`IRInstruction::LoadConst`].

use expo_ast::util::dedent;
use expo_ir_eval::{EnumPayload, Value};

mod common;

fn evaluate_script(source: &str) -> Value {
    common::evaluate_script(source).expect("interpreter should not error on this fixture")
}

#[test]
fn primitive_constant_is_visible_in_script() {
    let source = "
        const N = 42

        N
        ";

    let value = evaluate_script(&dedent(source));
    assert_eq!(value.as_int(), Some(42));
}

#[test]
fn struct_constant_field_access_computes() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        const ORIGIN = Point{x: 10, y: 32}

        ORIGIN.x + ORIGIN.y
        ";

    let value = evaluate_script(&dedent(source));
    assert_eq!(value.as_int(), Some(42));
}

#[test]
fn unit_enum_constant_materializes() {
    let source = "
        enum Axis
          X
        end

        const PRIMARY = Axis.X

        PRIMARY
        ";

    let value = evaluate_script(&dedent(source));
    let Value::Enum {
        name, tag, payload, ..
    } = value
    else {
        panic!("expected Value::Enum, got {value:?}");
    };
    assert_eq!(name, "X");
    assert_eq!(tag.0, 0);
    assert_eq!(payload, EnumPayload::Unit);
}
