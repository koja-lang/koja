//! Lowering coverage for string literals: `ExprKind::String { parts:
//! [Literal] }` → `IRInstruction::Const { ConstValue::String }` with
//! return type `IRType::String`. Interpolation surfaces as a
//! feature-gap diagnostic.

use koja_ast::util::dedent;
use koja_ir::{ConstValue, IRInstruction, IRTerminator, IRType};

mod common;

use common::{function, lower_program_source as lower};

#[test]
fn string_literal_lowers_to_const_string() {
    let source = "
        fn main -> String
          \"hello\"
        end
        ";

    let program = lower(&dedent(source));
    let main = function(&program, "main");
    assert_eq!(main.return_type, IRType::String);

    let block = main.blocks.first().expect("main has at least one block");
    assert_eq!(block.instructions.len(), 1);

    let IRInstruction::Const { dest, value } = &block.instructions[0] else {
        panic!(
            "expected a Const instruction, got {:?}",
            block.instructions[0]
        );
    };
    let ConstValue::String(text) = value else {
        panic!("expected ConstValue::String, got {value:?}");
    };
    assert_eq!(text, "hello");

    assert_eq!(
        block.terminator,
        IRTerminator::Return { value: Some(*dest) },
    );
}

#[test]
fn empty_string_literal_lowers_to_empty_const_string() {
    let source = "
        fn main -> String
          \"\"
        end
        ";

    let program = lower(&dedent(source));
    let main = function(&program, "main");
    let block = main.blocks.first().expect("main has at least one block");

    let IRInstruction::Const { value, .. } = &block.instructions[0] else {
        panic!("expected a Const instruction");
    };
    let ConstValue::String(text) = value else {
        panic!("expected ConstValue::String, got {value:?}");
    };
    assert!(text.is_empty(), "expected empty string, got {text:?}");
}
