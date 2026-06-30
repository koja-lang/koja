//! End-to-end eval coverage for nested type names: a nested struct
//! constructs / formats, a method on a nested struct dispatches, and
//! Debug renders the full surface name (`Owner.Nested`,
//! `Enum.Variant`) rather than just the leaf.

use koja_ir_eval::Value;

mod common;

use common::{evaluate_program, evaluate_script as evaluate};

fn run_string(source: &str) -> String {
    match evaluate(source).expect("evaluation should succeed") {
        Value::String(bytes) => String::from_utf8(bytes.to_vec()).expect("utf8"),
        other => panic!("expected `Value::String`, got `{other}`"),
    }
}

fn run_main_int(source: &str) -> i64 {
    match evaluate_program(source).expect("evaluation should succeed") {
        Value::Int(n) => n,
        other => panic!("expected `Value::Int`, got `{other}`"),
    }
}

#[test]
fn nested_struct_formats_with_surface_name() {
    let out = run_string(
        "
        struct Outer
          tag: Int
        end

        struct Outer.Inner
          x: Int
        end

        Outer.Inner{x: 5}.format()
        ",
    );
    assert_eq!(out, "Outer.Inner{x: 5}");
}

#[test]
fn method_on_nested_struct_dispatches() {
    let value = run_main_int(
        "
        struct Outer
          tag: Int
        end

        struct Outer.Inner
          x: Int
        end

        extend Outer.Inner
          fn doubled(self) -> Int
            self.x * 2
          end
        end

        fn main() -> Int
          Outer.Inner{x: 21}.doubled()
        end
        ",
    );
    assert_eq!(value, 42);
}

#[test]
fn deeply_nested_struct_formats_and_dispatches() {
    let out = run_string(
        "
        struct A
          tag: Int
        end

        struct A.B
          tag: Int
        end

        struct A.B.C
          x: Int
        end

        A.B.C{x: 7}.format()
        ",
    );
    assert_eq!(out, "A.B.C{x: 7}");

    let value = run_main_int(
        "
        struct A
          tag: Int
        end

        struct A.B
          tag: Int
        end

        struct A.B.C
          x: Int
        end

        extend A.B.C
          fn tripled(self) -> Int
            self.x * 3
          end
        end

        fn main() -> Int
          A.B.C{x: 14}.tripled()
        end
        ",
    );
    assert_eq!(value, 42);
}

#[test]
fn enum_struct_variant_formats_with_full_name() {
    let out = run_string(
        "
        enum Shape
          Rect{w: Int, h: Int}
          Circle
        end

        Shape.Rect{w: 2, h: 3}.format()
        ",
    );
    assert_eq!(out, "Shape.Rect{w: 2, h: 3}");
}

#[test]
fn enum_unit_variant_formats_with_full_name() {
    let out = run_string(
        "
        enum Shape
          Rect{w: Int, h: Int}
          Circle
        end

        Shape.Circle.format()
        ",
    );
    assert_eq!(out, "Shape.Circle");
}
