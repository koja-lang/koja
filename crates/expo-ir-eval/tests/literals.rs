//! Literal flow through the interpreter (`Int`, `String`; grows with
//! `Bool`, `Float`, `Binary` literals as coverage lands).
//!
//! Maps to LANGUAGE.md "Lexical Structure" -- literal sub-section.

use std::rc::Rc;

mod common;

use common::{dedent, eval_entry};
use expo_ir_eval::Value;

#[test]
fn evaluates_int_literal() {
    let source = "
        fn run -> Int
          42
        end
        ";

    let program = dedent(source);
    let value = eval_entry(&program, "run");
    assert_eq!(value, Value::Int(42));
}

#[test]
fn evaluates_string_literal() {
    // Pure-literal strings short-circuit through `resolve_const` ->
    // `IROperand::ConstStr` before the big lower_expr_to_operand match
    // fires, so this test confirms the const-folding path delivers a
    // `Value::String` to the interpreter.
    let source = "
        fn run -> String
          \"hello\"
        end
        ";

    let program = dedent(source);
    let value = eval_entry(&program, "run");
    assert_eq!(value, Value::String(Rc::new("hello".to_string())));
}

#[test]
fn evaluates_string_interpolation_int() {
    let source = "
        fn run -> String
          n = 42
          \"answer = #{n}\"
        end
        ";

    let program = dedent(source);
    let value = eval_entry(&program, "run");
    assert_eq!(value, Value::String(Rc::new("answer = 42".to_string())));
}

#[test]
fn evaluates_string_interpolation_string() {
    let source = "
        fn run -> String
          name = \"Alice\"
          \"hello, #{name}!\"
        end
        ";

    let program = dedent(source);
    let value = eval_entry(&program, "run");
    assert_eq!(value, Value::String(Rc::new("hello, Alice!".to_string())));
}

#[test]
fn evaluates_string_interpolation_multiple_holes() {
    let source = "
        fn run -> String
          a = 2
          b = 3
          \"#{a} + #{b} = #{a + b}\"
        end
        ";

    let program = dedent(source);
    let value = eval_entry(&program, "run");
    assert_eq!(value, Value::String(Rc::new("2 + 3 = 5".to_string())));
}

#[test]
fn evaluates_binary_literal_bytes() {
    let source = "
        fn run -> Binary
          <<0xFF, 0x00, 0xAB>>
        end
        ";

    let program = dedent(source);
    let value = eval_entry(&program, "run");
    assert_eq!(value, Value::Binary(Rc::new(vec![0xFF, 0x00, 0xAB])));
}

#[test]
fn evaluates_binary_literal_with_size() {
    let source = "
        fn run -> Binary
          <<256::16>>
        end
        ";

    let program = dedent(source);
    let value = eval_entry(&program, "run");
    assert_eq!(value, Value::Binary(Rc::new(vec![0x01, 0x00])));
}

#[test]
fn evaluates_binary_literal_string_segment() {
    let source = "
        fn run -> Binary
          <<\"hi\", 0x21>>
        end
        ";

    let program = dedent(source);
    let value = eval_entry(&program, "run");
    assert_eq!(value, Value::Binary(Rc::new(vec![0x68, 0x69, 0x21])));
}

// `const` reference path: primitives inline as `IROperand`, the
// rest pool as `IRConstantValue` and load via `LoadConst`.

#[test]
fn evaluates_inlined_int_const() {
    let source = "
        const ANSWER = 42

        fn run -> Int
          ANSWER
        end
        ";

    let program = dedent(source);
    let value = eval_entry(&program, "run");
    assert_eq!(value, Value::Int(42));
}

#[test]
fn evaluates_string_const() {
    let source = "
        const NAME = \"expo\"

        fn run -> String
          NAME
        end
        ";

    let program = dedent(source);
    let value = eval_entry(&program, "run");
    assert_eq!(value, Value::String(Rc::new("expo".to_string())));
}

#[test]
fn evaluates_enum_variant_const() {
    let source = "
        enum Direction
          North
          South
        end

        const HEADING = Direction.North

        fn run -> Direction
          HEADING
        end
        ";

    let program = dedent(source);
    let value = eval_entry(&program, "run");
    let Value::Enum(enum_value) = value else {
        panic!("expected Value::Enum, got {value:?}");
    };
    assert_eq!(enum_value.variant, "North");
    assert_eq!(enum_value.tag, 0);
}

#[test]
fn evaluates_struct_const_field_access() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        const ORIGIN = Point{x: 1, y: 2}

        fn run -> Int
          ORIGIN.x
        end
        ";

    let program = dedent(source);
    let value = eval_entry(&program, "run");
    assert_eq!(value, Value::Int(1));
}
