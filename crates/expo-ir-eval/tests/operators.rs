//! Operator evaluation: arithmetic today (Int + - * / %), comparisons
//! and logical operators as coverage grows.
//!
//! Operators don't have a dedicated LANGUAGE.md section -- they live
//! across "Variables and Constants" and the per-type sections in
//! "Types" -- but they form a coherent test cluster because the
//! expansion path (`==`/`!=` for enums + strings, `&&`/`||`/`!` for
//! bools, `<`/`<=`/`>`/`>=` for ints + floats) all flows through
//! [`expo_ir::resolved::ops`].

mod common;

use common::{dedent, eval_entry};
use expo_ir_eval::Value;

#[test]
fn evaluates_arithmetic() {
    let source = "
        fn run -> Int
          1 + 2 * 3
        end
        ";

    let program = dedent(source);
    let value = eval_entry(&program, "run");
    assert_eq!(value, Value::Int(7));
}

#[test]
fn evaluates_string_equality() {
    let source = "
        fn run -> Bool
          \"foo\" == \"foo\"
        end
        ";

    let program = dedent(source);
    let value = eval_entry(&program, "run");
    assert_eq!(value, Value::Bool(true));
}

#[test]
fn evaluates_string_inequality() {
    let source = "
        fn run -> Bool
          \"foo\" != \"bar\"
        end
        ";

    let program = dedent(source);
    let value = eval_entry(&program, "run");
    assert_eq!(value, Value::Bool(true));
}

#[test]
fn evaluates_string_concat() {
    use std::rc::Rc;

    let source = "
        fn run -> String
          \"foo\" <> \"bar\"
        end
        ";

    let program = dedent(source);
    let value = eval_entry(&program, "run");
    assert_eq!(value, Value::String(Rc::new("foobar".to_string())));
}

#[test]
fn evaluates_binary_concat() {
    use std::rc::Rc;

    let source = "
        fn run -> Binary
          a: Binary = <<0xAA>>
          b: Binary = <<0xBB, 0xCC>>
          a <> b
        end
        ";

    let program = dedent(source);
    let value = eval_entry(&program, "run");
    assert_eq!(value, Value::Binary(Rc::new(vec![0xAA, 0xBB, 0xCC])));
}

#[test]
fn evaluates_subtraction() {
    let source = "
        fn run -> Int
          10 - 4
        end
        ";

    let program = dedent(source);
    let value = eval_entry(&program, "run");
    assert_eq!(value, Value::Int(6));
}
