//! Enum construction across all payload shapes (unit, tuple, struct)
//! and generic enum instantiation.
//!
//! Maps to LANGUAGE.md "Types" -- enum sub-section.

mod common;

use common::{dedent, eval_entry};
use expo_ir_eval::{Value, VariantPayload};

#[test]
fn evaluates_struct_enum_construction() {
    let source = "
        enum Shape
          Rect { w: Int, h: Int }
          Circle { r: Int }
        end

        fn run -> Shape
          Shape.Rect { w: 10, h: 20 }
        end
        ";

    let program = dedent(source);
    let value = eval_entry(&program, "run");
    let Value::Enum(e) = value else {
        panic!("expected enum value, got {value:?}");
    };
    assert_eq!(e.mangled.as_str(), "__test__.Shape");
    assert_eq!(e.variant, "Rect");
    assert_eq!(e.tag, 0);
    let VariantPayload::Struct(fields) = &e.payload else {
        panic!("expected struct payload, got {:?}", e.payload);
    };
    assert_eq!(fields.len(), 2);
    assert_eq!(fields[0].0, "w");
    assert_eq!(fields[0].1, Value::Int(10));
    assert_eq!(fields[1].0, "h");
    assert_eq!(fields[1].1, Value::Int(20));
}

#[test]
fn evaluates_tuple_enum_construction() {
    // Generic enum construction: `Wrapper<Int>.Just(42)` exercises both
    // the closure pass (registering `Wrapper_$Int$`) and the tuple
    // payload path of the enum-construct lift.
    let source = "
        enum Wrapper<T>
          Just(T)
          Nothing
        end

        fn run -> Wrapper<Int>
          Wrapper.Just(42)
        end
        ";

    let program = dedent(source);
    let value = eval_entry(&program, "run");
    let Value::Enum(e) = value else {
        panic!("expected enum value, got {value:?}");
    };
    assert_eq!(e.mangled.as_str(), "__test__.Wrapper_$Int$");
    assert_eq!(e.variant, "Just");
    assert_eq!(e.tag, 0);
    let VariantPayload::Tuple(values) = &e.payload else {
        panic!("expected tuple payload, got {:?}", e.payload);
    };
    assert_eq!(values.as_slice(), &[Value::Int(42)]);
}

#[test]
fn evaluates_unit_enum_construction() {
    let source = "
        enum Color
          Red
          Green
          Blue
        end

        fn run -> Color
          Color.Green
        end
        ";

    let program = dedent(source);
    let value = eval_entry(&program, "run");
    let Value::Enum(e) = value else {
        panic!("expected enum value, got {value:?}");
    };
    assert_eq!(e.mangled.as_str(), "__test__.Color");
    assert_eq!(e.variant, "Green");
    assert_eq!(e.tag, 1);
    assert!(matches!(e.payload, VariantPayload::Unit));
}
