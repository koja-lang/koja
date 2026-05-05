//! End-to-end interpreter coverage for `if` and `unless`.
//!
//! `if` and `unless` are Unit-typed in this slice, so the headline
//! contract is observable through helper functions whose body uses
//! the conditional to gate an early `return`. The interpreter walks
//! both arms by dispatching on the `CondBranch` terminator at
//! runtime; cond=`true` selects the `then` block and cond=`false`
//! selects the `else` block. `unless` swaps those at lower time.
//!
//! Identifier references inside function bodies aren't resolved
//! until the locals slice, so each test pairs `pick_*_*` helpers
//! whose `if` / `unless` cond is a literal Bool. Two helpers per
//! shape exercise both arms; `pick_*_then` produces the early-return
//! value and `pick_*_merge` produces the fall-through value.

use std::path::PathBuf;

use expo_alpha_ir::{lower_program, lower_script};
use expo_alpha_ir_eval::{Interpreter, RuntimeError, Value};
use expo_alpha_typecheck::{CheckedProgram, check_program};
use expo_ast::identifier::Identifier;
use expo_ast::util::dedent;
use expo_parser::{ParseMode, SourceFile, parse_program};

const PACKAGE: &str = "TestApp";

fn typecheck(source: &str, mode: ParseMode) -> CheckedProgram {
    let parsed = parse_program(
        vec![SourceFile {
            package: PACKAGE.to_string(),
            path: PathBuf::from("branches.expo"),
            source: source.to_string(),
        }],
        mode,
    );
    check_program(parsed).unwrap_or_else(|failure| panic!("alpha typecheck failed:\n{failure}"))
}

fn evaluate(source: &str) -> Result<Value, RuntimeError> {
    let checked = typecheck(source, ParseMode::File);
    let entry = Identifier::new(PACKAGE, vec!["main".to_string()]);
    let program = lower_program(&checked, entry).expect("alpha lowering should succeed");
    Interpreter::run_program(program)
}

fn evaluate_script(source: &str) -> Result<Value, RuntimeError> {
    let checked = typecheck(source, ParseMode::Script);
    let script = lower_script(&checked).expect("alpha script lowering should succeed");
    Interpreter::run_script(script)
}

#[test]
fn if_with_true_condition_executes_then_branch() {
    // The early `return 1` inside the `if true` body fires; the
    // merge block's trailing `2` is unreachable.
    let source = "
        fn pick -> Int
          if true
            return 1
          end
          2
        end

        pick()
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(1));
}

#[test]
fn if_with_false_condition_falls_through_to_merge() {
    // The cond evaluates to `false`, so the then-block is skipped
    // entirely; the trailing `2` in the merge block is the
    // function's return value.
    let source = "
        fn pick -> Int
          if false
            return 1
          end
          2
        end

        pick()
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(2));
}

#[test]
fn unless_with_false_condition_executes_body() {
    // `unless cond` runs the body when cond is `false`. The early
    // `return 1` therefore fires when the cond is the literal
    // `false`.
    let source = "
        fn pick -> Int
          unless false
            return 1
          end
          2
        end

        pick()
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(1));
}

#[test]
fn unless_with_true_condition_skips_body() {
    let source = "
        fn pick -> Int
          unless true
            return 1
          end
          2
        end

        pick()
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(2));
}

#[test]
fn if_drives_program_mode_through_helper_calls() {
    // Mirror of the script-mode coverage above, exercising the
    // project-mode entry path. Each helper exercises a different
    // arm of the same `if` shape and `main` sums them.
    let source = "
        fn pick_then -> Int
          if true
            return 1
          end
          2
        end

        fn pick_merge -> Int
          if false
            return 1
          end
          2
        end

        fn main
          pick_then() + pick_merge()
        end
        ";
    assert_eq!(evaluate(&dedent(source)).unwrap(), Value::Int(3));
}
