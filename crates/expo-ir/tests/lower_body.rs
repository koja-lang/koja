//! Coverage for the statement-list driver in `src/lower/body.rs`:
//! `Statement::Return` shape — both `return <expr>` and bare
//! `return` (Unit) — pinning the terminator that gets stamped and
//! the closed-flow contract; the empty-body Unit-return shape.
//!
//! Per-function fail-fast within the body is exercised end-to-end
//! by `lower_package.rs:partial_failure_reports_only_the_failing_function_diagnostic`.

use expo_ast::util::dedent;
use expo_ir::{IRInstruction, IRTerminator, IRType};

mod common;

use common::{function, lower_program_source as lower};

#[test]
fn explicit_return_with_value_terminates_block() {
    let source = "
        fn main
          return 7
        end
        ";

    let program = lower(&dedent(source));
    let main = function(&program, "main");
    assert_eq!(
        main.blocks.len(),
        1,
        "a single explicit `return` produces one block",
    );

    let block = &main.blocks[0];
    let last = block.instructions.last().expect("expected a Const for `7`");
    let dest = last.dest().expect("Const produces a value");
    assert!(
        matches!(last, IRInstruction::Const { .. }),
        "trailing instruction should be Const(7); got {last:?}",
    );
    assert_eq!(block.terminator, IRTerminator::Return { value: Some(dest) });
}

#[test]
fn empty_main_body_returns_unit_with_no_value() {
    let source = "
        fn main
        end
        ";

    let program = lower(&dedent(source));
    let main = function(&program, "main");
    assert_eq!(main.return_type, IRType::Unit);
    let block = main.blocks.first().expect("main has at least one block");
    assert!(
        block.instructions.is_empty(),
        "an empty body should not emit any instructions; got {:?}",
        block.instructions,
    );
    assert_eq!(block.terminator, IRTerminator::Return { value: None });
}
