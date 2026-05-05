//! Runtime coverage for the struct slice in
//! [`expo_alpha_ir_eval::Interpreter`]: `IRInstruction::StructInit`
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

use expo_alpha_ir_eval::Value;
use expo_ast::util::dedent;

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
    assert_eq!(fields[2], Value::String("jules".to_string()));
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
