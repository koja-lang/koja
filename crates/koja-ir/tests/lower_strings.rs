//! Lowering coverage for string literals: `ExprKind::String { parts:
//! [Literal] }` → `IRInstruction::Const { ConstValue::String }` with
//! return type `IRType::String`. Interpolation surfaces as a
//! feature-gap diagnostic.
//!
//! Returning the literal is an ownership boundary, so the rc baseline
//! *acquires* it: a [`IRInstruction::Clone`] (`rc++`, a no-op on the
//! immortal rodata literal) follows the `Const`, and the `Return`
//! targets the clone.

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

    let IRInstruction::Clone {
        dest: cloned,
        source,
        ..
    } = &block.instructions[1]
    else {
        panic!(
            "expected the literal to be acquired by a Clone, got {:?}",
            block.instructions[1]
        );
    };
    assert_eq!(source, dest, "the Clone should acquire the Const literal");

    assert_eq!(
        block.terminator,
        IRTerminator::Return {
            value: Some(*cloned)
        },
        "Return should target the acquired (cloned) value",
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
