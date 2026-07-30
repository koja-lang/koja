//! Coverage for the statement-list driver in `src/lower/body.rs`:
//! the `Statement::Return` shape (both `return <expr>` and bare
//! Unit `return`) pinning the terminator that gets stamped and the
//! closed-flow contract, plus the empty-body Unit-return shape.
//!
//! Per-function fail-fast within the body is exercised end-to-end
//! by `lower_package.rs:partial_failure_reports_only_the_failing_function_diagnostic`.

use koja_ir::{IRInstruction, IRTerminator, IRType};

mod common;

use common::{entry_block, lower_script_source as lower, script_function};

#[test]
fn explicit_return_with_value_terminates_block() {
    let script = lower(
        "
        fn pick(flag: Bool) -> Int
          if flag
            return 7
          end
          0
        end

        pick(true).print()
        ",
    );
    let function = script_function(&script, "pick");
    let return_blocks: Vec<_> = function
        .blocks
        .iter()
        .filter(|block| matches!(block.terminator, IRTerminator::Return { value: Some(_) }))
        .collect();
    assert_eq!(
        return_blocks.len(),
        2,
        "the early `return 7` and the fallthrough `0` each terminate their own block",
    );
    for block in return_blocks {
        let last = block
            .instructions
            .last()
            .expect("expected a trailing Const");
        assert!(
            matches!(last, IRInstruction::Const { .. }),
            "trailing instruction should be a Const; got {last:?}",
        );
        let dest = last.dest().expect("Const produces a value");
        assert_eq!(block.terminator, IRTerminator::Return { value: Some(dest) });
    }
}

#[test]
fn script_bare_return_terminates_block() {
    let script = lower("return\n");
    let block = entry_block(&script.blocks);
    assert_eq!(block.terminator, IRTerminator::Return { value: None });
}

#[test]
fn empty_main_body_returns_unit_with_no_value() {
    let script = lower("\n");
    assert_eq!(script.return_type, IRType::Unit);
    let block = entry_block(&script.blocks);
    assert!(
        block.instructions.is_empty(),
        "an empty body should not emit any instructions; got {:?}",
        block.instructions,
    );
    assert_eq!(block.terminator, IRTerminator::Return { value: None });
}
