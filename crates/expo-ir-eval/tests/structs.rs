//! Runtime coverage for the struct slice in
//! [`expo_ir_eval::Interpreter`]: `IRInstruction::StructInit`
//! materializes a [`Value::Struct`] with positional fields, and
//! `IRInstruction::FieldGet` projects a field by index without
//! re-cloning the receiver. Mixed-type fields (Int + Bool + String)
//! and nested structs (a struct held by another struct) exercise
//! the cross-type and recursive paths.
//!
//! The script-mode path (no `fn main` wrapper) is the unit under test
//! because the trailing expression's runtime [`Value`] becomes the
//! script's return value, which is exactly what we want to inspect.
//! Project-mode coverage of the same instruction set lives in
//! `tests/interpreter.rs`.

use expo_ast::util::dedent;
use expo_ir_eval::Value;

mod common;

fn evaluate_script(source: &str) -> Value {
    common::evaluate_script(source).expect("interpreter should not error on this fixture")
}

#[test]
fn struct_construction_yields_value_struct_with_positional_fields() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        Point{x: 5, y: 10}
        ";

    let value = evaluate_script(&dedent(source));
    let Value::Struct { symbol, fields } = value else {
        panic!("expected Value::Struct, got {value:?}");
    };
    assert_eq!(symbol.mangled(), "TestApp.Point");
    assert_eq!(fields, vec![Value::Int(5), Value::Int(10)]);
}

#[test]
fn struct_field_access_projects_declared_field() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        Point{x: 5, y: 10}.x
        ";

    let value = evaluate_script(&dedent(source));
    assert_eq!(value, Value::Int(5));
}

#[test]
fn struct_field_access_projects_second_field() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        Point{x: 5, y: 10}.y
        ";

    let value = evaluate_script(&dedent(source));
    assert_eq!(value, Value::Int(10));
}

#[test]
fn struct_with_mixed_type_fields_projects_each_field() {
    let source = "
        struct Profile
          age: Int
          active: Bool
          handle: String
        end

        Profile{age: 30, active: true, handle: \"jules\"}
        ";

    let value = evaluate_script(&dedent(source));
    let Value::Struct { fields, .. } = value else {
        panic!("expected Value::Struct, got {value:?}");
    };
    assert_eq!(fields[0], Value::Int(30));
    assert_eq!(fields[1], Value::Bool(true));
    assert_eq!(fields[2], Value::String("jules".into()));
}

#[test]
fn mixed_type_struct_field_access_returns_each_concrete_type() {
    let source = "
        struct Profile
          age: Int
          active: Bool
          handle: String
        end

        Profile{age: 30, active: true, handle: \"jules\"}.active
        ";

    let value = evaluate_script(&dedent(source));
    assert_eq!(value, Value::Bool(true));
}

#[test]
fn nested_struct_field_holds_inner_value_struct() {
    let source = "
        struct Inner
          n: Int
        end

        struct Outer
          inner: Inner
          tag: Bool
        end

        Outer{inner: Inner{n: 42}, tag: false}
        ";

    let value = evaluate_script(&dedent(source));
    let Value::Struct { symbol, fields } = value else {
        panic!("expected outer Value::Struct, got {value:?}");
    };
    assert_eq!(symbol.mangled(), "TestApp.Outer");
    let Value::Struct {
        symbol: inner_symbol,
        fields: inner_fields,
    } = &fields[0]
    else {
        panic!("expected inner Value::Struct, got {:?}", fields[0]);
    };
    assert_eq!(inner_symbol.mangled(), "TestApp.Inner");
    assert_eq!(inner_fields, &vec![Value::Int(42)]);
    assert_eq!(fields[1], Value::Bool(false));
}

#[test]
fn nested_struct_field_chain_projects_inner_int() {
    let source = "
        struct Inner
          n: Int
        end

        struct Outer
          inner: Inner
          tag: Bool
        end

        Outer{inner: Inner{n: 42}, tag: false}.inner.n
        ";

    let value = evaluate_script(&dedent(source));
    assert_eq!(value, Value::Int(42));
}

// ---------------------------------------------------------------------------
// Static methods (inline + impl-block forms)
// ---------------------------------------------------------------------------

#[test]
fn inline_static_method_call_then_field_get_evaluates() {
    // Inline-form static method: `Point.origin()` returns the
    // struct, then the trailing `.x` field-projects to `0`.
    let source = "
        struct Point
          x: Int
          y: Int

          fn origin -> Point
            Point{x: 0, y: 0}
          end
        end

        Point.origin().x
        ";

    let value = evaluate_script(&dedent(source));
    assert_eq!(value, Value::Int(0));
}

#[test]
fn impl_block_static_method_call_then_field_get_evaluates() {
    // Impl-form: same source as above, just declared in an
    // `impl Point` block. Same runtime answer pins that the two
    // surface forms produce identical IR (and therefore identical
    // eval behavior).
    let source = "
        struct Point
          x: Int
          y: Int
        end

        impl Point
          fn origin -> Point
            Point{x: 0, y: 0}
          end
        end

        Point.origin().x
        ";

    let value = evaluate_script(&dedent(source));
    assert_eq!(value, Value::Int(0));
}

#[test]
fn static_method_returning_int_evaluates_to_constant() {
    // Const-bodied method. Pin that the call dispatches to the
    // right symbol and the return value flows back.
    let source = "
        struct Point
          x: Int

          fn answer -> Int
            42
          end
        end

        Point.answer()
        ";

    let value = evaluate_script(&dedent(source));
    assert_eq!(value, Value::Int(42));
}

// ---------------------------------------------------------------------------
// Instance methods (inline + impl-block forms)
// ---------------------------------------------------------------------------

#[test]
fn inline_instance_method_reads_self_field() {
    let source = "
        struct Point
          x: Int
          y: Int

          fn first(self) -> Int
            self.x
          end
        end

        Point{x: 7, y: 3}.first()
        ";

    let value = evaluate_script(&dedent(source));
    assert_eq!(value, Value::Int(7));
}

#[test]
fn impl_block_instance_method_reads_self_field() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        impl Point
          fn second(self) -> Int
            self.y
          end
        end

        Point{x: 7, y: 3}.second()
        ";

    let value = evaluate_script(&dedent(source));
    assert_eq!(value, Value::Int(3));
}

#[test]
fn instance_method_with_explicit_arg_combines_self_and_arg() {
    let source = "
        struct Counter
          n: Int

          fn add(self, delta: Int) -> Int
            self.n + delta
          end
        end

        Counter{n: 10}.add(5)
        ";

    let value = evaluate_script(&dedent(source));
    assert_eq!(value, Value::Int(15));
}

#[test]
fn instance_method_self_field_then_field_get_chains() {
    let source = "
        struct Inner
          n: Int
        end

        struct Outer
          inner: Inner

          fn unwrap(self) -> Inner
            self.inner
          end
        end

        Outer{inner: Inner{n: 42}}.unwrap().n
        ";

    let value = evaluate_script(&dedent(source));
    assert_eq!(value, Value::Int(42));
}
