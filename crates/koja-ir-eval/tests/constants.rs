//! Runtime coverage for constants. Primitives inline and pooled
//! compounds materialize through [`IRInstruction::LoadConst`].
//! Constants nested under a type (`Span.ZERO`) lower the same way as
//! package constants once desugar hoists them.

use koja_ast::util::dedent;
use koja_ir_eval::{EnumPayload, Value};

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

const DEP_CONSTANTS: &str = "
    const MAX = 40
    const default_size = 2
    const BANNER = \"koja\"
    ";

fn evaluate_with_dep(script: &str) -> Value {
    common::evaluate_script_with_dep("Dep", &dedent(DEP_CONSTANTS), &dedent(script))
        .expect("interpreter should not error on this fixture")
}

#[test]
fn cross_package_primitive_constants_inline() {
    let value = evaluate_with_dep("Dep.MAX + Dep.default_size");
    assert_eq!(value.as_int(), Some(42));
}

#[test]
fn cross_package_compound_constant_materializes() {
    let value = evaluate_with_dep("\"#{Dep.BANNER}!\"");
    assert_eq!(value.as_string(), Some("koja!"));
}

const NESTED_CONSTANTS: &str = "
    struct Span
      value: Int
      unit: Span.Unit

      enum Unit
        Seconds
        Minutes
      end

      const ZERO = Span{value: 0, unit: Span.Unit.Seconds}
      const limit = 100

      fn zero?(self) -> Bool
        self.value == 0
      end
    end

    const Span.MAX = Span{value: 99, unit: Span.Unit.Minutes}

    enum Color
      Red
      Green

      const DEFAULT = Color.Green
    end

    struct Summary
      elapsed: Span = Span.ZERO
      count: Int = Span.limit
      color: Color = Color.DEFAULT
    end
    ";

fn evaluate_nested(tail: &str) -> Value {
    evaluate_script(&format!("{}\n{}", dedent(NESTED_CONSTANTS), dedent(tail)))
}

#[test]
fn nested_constants_read_through_the_owner() {
    let value = evaluate_nested("Span.MAX.value + Span.limit");
    assert_eq!(value.as_int(), Some(199));
}

#[test]
fn nested_constant_receives_method_calls() {
    let value = evaluate_nested("Span.ZERO.zero?()");
    assert_eq!(value.as_bool(), Some(true));
}

#[test]
fn nested_struct_constant_carries_its_enum_field() {
    let value = evaluate_nested("Span.MAX.unit == Span.Unit.Minutes");
    assert_eq!(value.as_bool(), Some(true));
}

#[test]
fn nested_constants_fill_field_defaults() {
    let value = evaluate_nested("s = Summary{}\ns.elapsed.value + s.count");
    assert_eq!(value.as_int(), Some(100));
}

#[test]
fn nested_enum_constant_fills_field_default() {
    let value = evaluate_nested("Summary{}.color == Color.Green");
    assert_eq!(value.as_bool(), Some(true));
}

#[test]
fn nested_constant_from_another_package() {
    let dep = "
        struct Span
          value: Int

          const ZERO = Span{value: 0}
          priv const hidden = 1
        end
        ";
    let value = common::evaluate_script_with_dep("Dep", &dedent(dep), "Dep.Span.ZERO.value")
        .expect("interpreter should not error on this fixture");
    assert_eq!(value.as_int(), Some(0));
}
