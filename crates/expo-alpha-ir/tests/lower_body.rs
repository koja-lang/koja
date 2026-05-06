//! Coverage for the statement-list driver in `src/lower/body.rs`:
//!
//! - `Statement::Return` shape — both `return <expr>` and bare
//!   `return` (Unit) — pinning the terminator that gets stamped and
//!   the closed-flow contract;
//! - per-function fail-fast within the body itself: an unsupported
//!   `Statement::Assignment` halts the body walk after one
//!   diagnostic, and a body that mixes feature-gaps emits exactly
//!   one diagnostic for whichever gap trips first (no cascading).

use expo_alpha_ir::{IRInstruction, IRTerminator, IRType};
use expo_ast::util::dedent;

mod common;

use common::{
    expect_diagnostics, function, lower_program_err as lower_err, lower_program_source as lower,
};

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

/// Multiple feature gaps inside a single function should emit *one*
/// diagnostic — the first one seen — and abort walking that function.
/// `break` still passes typecheck (no loop context check in alpha
/// yet) but is an IR-level feature gap, so it's the lowest-friction
/// way to trip the fail-fast contract through IR lower without
/// short-circuiting at typecheck.
#[test]
fn fail_fast_within_function_emits_single_diagnostic() {
    let source = "
        fn main
          break
          break
        end
        ";

    let program = dedent(source);
    let messages = expect_diagnostics(lower_err(&program, "main"));
    assert_eq!(
        messages.len(),
        1,
        "expected fail-fast within a function, got: {messages:?}",
    );
    assert!(
        messages[0].contains("`break` statements"),
        "expected first diagnostic to be the break gap, got: {messages:?}",
    );
}
