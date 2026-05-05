//! Coverage for `@intrinsic` lowering in `src/lower/package.rs`:
//!
//! - a bodyless `@intrinsic` decl lowers to an [`IRFunction`] with
//!   [`FunctionKind::Intrinsic`] and zero basic blocks (the backend
//!   synthesizes the body at emit time);
//! - a call to an `@intrinsic` symbol lowers to an ordinary
//!   [`IRInstruction::Call`] — same shape as a call to a regular
//!   helper, distinguished only by the callee's `kind` on the
//!   resolved [`IRFunction`];
//! - the seal pass admits the empty-blocks shape for `Intrinsic`
//!   without panicking on the "function has no basic blocks"
//!   contract that gates `Regular` fns.
//!
//! Each fixture parses + typechecks + lowers in script-mode so the
//! `@intrinsic fn print(s: String)` decl lives alongside the body
//! that calls it; project-mode (program) coverage waits on the
//! stdlib-loading slice that imports intrinsics from real `lib/`
//! files.
//!
//! [`FunctionKind`]: expo_alpha_ir::FunctionKind

use expo_alpha_ir::{FunctionKind, IRInstruction, IRScript, IRTerminator, IRType};
use expo_ast::util::dedent;
use expo_parser::ParseMode;

mod common;

const PACKAGE: &str = "Global";

fn lower(source: &str) -> IRScript {
    common::lower_script_source_in(PACKAGE, source)
}

#[test]
fn intrinsic_fn_lowers_to_function_kind_intrinsic_with_empty_blocks() {
    let source = "
        @intrinsic
        fn print(s: String)
        ";

    let script = lower(&dedent(source));
    let mangled = format!("{PACKAGE}.print");
    let function = script
        .function(&mangled)
        .unwrap_or_else(|| panic!("missing intrinsic `{mangled}` in IRScript"));

    assert_eq!(function.kind, FunctionKind::Intrinsic);
    assert!(
        function.blocks.is_empty(),
        "intrinsic body should lower to zero blocks; got {} block(s)",
        function.blocks.len(),
    );
    assert_eq!(function.return_type, IRType::Unit);
    assert_eq!(function.params.len(), 1);
    assert_eq!(function.params[0].ty, IRType::String);
}

#[test]
fn intrinsic_call_lowers_to_normal_call_instruction() {
    let source = "
        @intrinsic
        fn print(s: String)

        print(\"hello\")
        ";

    let script = lower(&dedent(source));
    let mangled = format!("{PACKAGE}.print");
    assert_eq!(script.return_type, IRType::Unit);

    let block = script.blocks.first().expect("script body has one block");
    let call = block
        .instructions
        .iter()
        .find_map(|inst| match inst {
            IRInstruction::Call { callee, args, dest } => Some((callee, args, dest)),
            _ => None,
        })
        .expect("expected a Call instruction in the script body");
    assert_eq!(call.0.mangled(), mangled);
    assert_eq!(call.1.len(), 1, "print takes exactly one String arg");
    assert_eq!(
        block.terminator,
        IRTerminator::Return {
            value: Some(*call.2),
        },
        "trailing intrinsic call's dest threads through to Return",
    );
}

#[test]
fn regular_function_still_lowers_to_kind_regular_with_blocks() {
    let source = "
        fn helper -> Int
          1
        end

        helper()
        ";

    let script = lower(&dedent(source));
    let mangled = format!("{PACKAGE}.helper");
    let function = script.function(&mangled).expect("helper should be lowered");
    assert_eq!(function.kind, FunctionKind::Regular);
    assert!(
        !function.blocks.is_empty(),
        "regular fn should carry at least one basic block",
    );
}

/// Forward-compat: even when no body is provided, the lowered
/// intrinsic must still surface through the script's
/// `script.function()` lookup so the LLVM dispatch and the
/// interpreter dispatch can find it by mangled symbol.
#[test]
fn lowered_intrinsic_is_visible_via_function_lookup() {
    let source = "
        @intrinsic
        fn print(s: String)
        ";

    let script = lower(&dedent(source));
    assert!(
        script.function(&format!("{PACKAGE}.print")).is_some(),
        "intrinsic should be reachable via IRScript::function lookup",
    );
}

/// Mismatched arg types at an intrinsic call site surface through
/// the typecheck pipeline (same path as regular fns) — the IR layer
/// doesn't special-case intrinsics. Pins that intrinsic signatures
/// participate in arg-type checking like any other function.
#[test]
fn intrinsic_arg_type_mismatch_diagnoses_through_typecheck() {
    let source = "
        @intrinsic
        fn print(s: String)

        print(1)
        ";

    let failure = common::typecheck_fail_in(PACKAGE, &dedent(source), ParseMode::Script);
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("expects `String`")),
        "expected typecheck arg-mismatch, got: {:?}",
        failure.diagnostics,
    );
}
