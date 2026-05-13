//! Coverage for the package- and function-shaped lowering entry
//! points (`src/lower/package.rs`):
//!
//! - happy-path lowering of a `fn main` (project mode) and a bare
//!   trailing-expression script body, asserting the produced
//!   instruction sequence on the resulting [`IRProgram`] /
//!   [`IRScript`];
//! - cross-package call wiring through script mode's
//!   [`IRScript::function`] index;
//! - the [`LowerError::EntryPointNotFound`] failure mode for a
//!   missing `fn main`;
//! - the per-function fail-fast contract: extern-fn-without-body
//!   surfaces a feature-gap diagnostic, and a single bad function
//!   in a multi-fn package produces exactly one diagnostic for the
//!   failing function while the rest are still walked.

use expo_alpha_ir::{
    ConstValue, IRBasicBlock, IRBinOp, IRFunction, IRInstruction, IRTerminator, IRType, LowerError,
    ValueId, lower_program,
};
use expo_ast::identifier::Identifier;
use expo_ast::util::dedent;
use expo_parser::ParseMode;

mod common;

use common::{
    PACKAGE, expect_diagnostics, lower_program_err as lower_err, lower_program_source as lower,
    lower_script_source as lower_as_script, typecheck,
};

#[test]
fn fn_main_two_plus_two_lowers_to_const_const_add_return() {
    let source = "
        fn main
          2 + 2
        end
        ";

    let program = lower(&dedent(source));

    assert_eq!(program.entry_point.mangled(), format!("{PACKAGE}.main"));
    // The user-facing `TestApp` package has just `main`; the
    // alpha auto-import seeds `Global` packages alongside it
    // (today `Global.bitwise` + `Global.time`), so we assert on
    // the user package's shape only and let the stdlib packages
    // ride along.
    let pkg = program
        .packages
        .iter()
        .find(|pkg| pkg.package == PACKAGE)
        .expect("test package missing from lowered IR");
    assert_eq!(pkg.functions.len(), 1);

    let main: &IRFunction = program.entry_function();
    assert_eq!(main.symbol, program.entry_point);
    assert_eq!(main.blocks.len(), 1, "fns lower to one basic block");

    let block: &IRBasicBlock = &main.blocks[0];
    assert_eq!(
        block.instructions,
        vec![
            IRInstruction::Const {
                dest: ValueId(0),
                value: ConstValue::Int64(2),
            },
            IRInstruction::Const {
                dest: ValueId(1),
                value: ConstValue::Int64(2),
            },
            IRInstruction::BinaryOp {
                dest: ValueId(2),
                lhs: ValueId(0),
                op: IRBinOp::Add,
                rhs: ValueId(1),
            },
        ],
    );
    assert_eq!(
        block.terminator,
        IRTerminator::Return {
            value: Some(ValueId(2)),
        },
    );
}

#[test]
fn bare_two_plus_two_lowers_to_script_with_const_const_add_return() {
    let script = lower_as_script("2 + 2\n");

    assert_eq!(
        script.return_type,
        IRType::Int64,
        "trailing `2 + 2` should stamp Int64 on IRScript.return_type",
    );
    let user_pkg = script
        .packages
        .iter()
        .find(|pkg| pkg.package == PACKAGE)
        .expect("test package missing from lowered IRScript");
    assert!(
        user_pkg.functions.is_empty(),
        "no helper fns expected in this fixture's user package; got {:?}",
        user_pkg,
    );
    assert_eq!(
        script.blocks.len(),
        1,
        "script bodies lower to a single basic block",
    );

    let block = &script.blocks[0];
    assert_eq!(
        block.instructions,
        vec![
            IRInstruction::Const {
                dest: ValueId(0),
                value: ConstValue::Int64(2),
            },
            IRInstruction::Const {
                dest: ValueId(1),
                value: ConstValue::Int64(2),
            },
            IRInstruction::BinaryOp {
                dest: ValueId(2),
                lhs: ValueId(0),
                op: IRBinOp::Add,
                rhs: ValueId(1),
            },
        ],
    );
    assert_eq!(
        block.terminator,
        IRTerminator::Return {
            value: Some(ValueId(2)),
        },
    );
}

#[test]
fn script_with_helper_fn_lowers_call_through_packages() {
    let script = lower_as_script("fn helper -> Int\n  1\nend\n\nhelper() + 1\n");

    let helper_mangled = format!("{PACKAGE}.helper");
    let helper = script
        .function(&helper_mangled)
        .expect("helper fn should be lowered into IRScript.packages");
    assert_eq!(helper.symbol.mangled(), helper_mangled);
    assert_eq!(helper.return_type, IRType::Int64);

    assert_eq!(script.return_type, IRType::Int64);
    let block = &script.blocks[0];
    let trailing = block
        .instructions
        .last()
        .expect("script body should produce at least one instruction");
    assert!(
        matches!(
            trailing,
            IRInstruction::BinaryOp {
                op: IRBinOp::Add,
                ..
            },
        ),
        "expected trailing BinaryOp(Add), got {trailing:?}",
    );
    let call = block
        .instructions
        .iter()
        .find(|inst| matches!(inst, IRInstruction::Call { .. }))
        .expect("expected a Call instruction in the script body");
    let IRInstruction::Call { callee, .. } = call else {
        unreachable!();
    };
    assert_eq!(callee.mangled(), helper_mangled);
}

#[test]
fn lower_program_reports_missing_entry_point() {
    let source = "
        fn other
          1
        end
        ";

    let checked = typecheck(&dedent(source), ParseMode::File);
    let missing = Identifier::new(PACKAGE, vec!["main".to_string()]);
    let err = lower_program(&checked, missing.clone())
        .expect_err("missing entry point should be reported");
    match err {
        LowerError::EntryPointNotFound { identifier } => assert_eq!(identifier, missing),
        other => panic!("expected EntryPointNotFound, got {other:?}"),
    }
}

/// Extern fns no longer surface a lower-time feature gap — they
/// lower to [`expo_alpha_ir::FunctionKind::Extern`] with empty
/// blocks. Type-shape rejections are intercepted earlier by the
/// alpha-typecheck FFI gate (see `expo-alpha-typecheck`'s
/// `extern_c.rs`). This test pins the lower-time positive path:
/// FFI-admissible signatures lower cleanly with the empty-body
/// shape the seal pass requires for `FunctionKind::Extern`.
#[test]
fn extern_fn_lowers_with_empty_blocks_and_extern_kind() {
    let source = "
        @extern \"C\"
        fn cosf(x: Float32) -> Float32

        fn main -> Int
          1
        end
        ";

    let program = lower(&dedent(source));
    let cosf = common::function(&program, "cosf");
    assert!(
        cosf.blocks.is_empty(),
        "extern fn should lower to zero blocks; got {}",
        cosf.blocks.len(),
    );
    assert!(
        matches!(cosf.kind, expo_alpha_ir::FunctionKind::Extern(_)),
        "expected FunctionKind::Extern for cosf; got {:?}",
        cosf.kind,
    );
}

/// When one function fails to lower, other functions in the same
/// package still get walked — the failing one is simply omitted, and
/// the final [`LowerError::Diagnostics`] carries *only* the diagnostic
/// from the function that actually failed. Pins the per-function
/// fail-fast contract so a single bad function doesn't mask issues in
/// other ones and doesn't spew spurious errors either. Uses a
/// bodyless `fn broken` (an IR-only feature gap that passes alpha
/// typecheck and parse — see `decl.rs:485` — but which IR lower
/// rejects with the bodyless-fn diagnostic in `package.rs:284`).
#[test]
fn partial_failure_reports_only_the_failing_function_diagnostic() {
    let source = "
        fn broken
        fn main
          1
        end
        ";

    let program = dedent(source);
    let messages = expect_diagnostics(lower_err(&program, "main"));
    assert_eq!(
        messages.len(),
        1,
        "expected a single diagnostic from the failing fn, got: {messages:?}",
    );
    assert!(
        messages[0].contains("bodyless fn") && messages[0].contains("broken"),
        "expected bodyless-fn diagnostic mentioning `broken`, got: {messages:?}",
    );
}
