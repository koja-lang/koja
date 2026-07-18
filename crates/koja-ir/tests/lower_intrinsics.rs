//! Coverage for `@intrinsic` lowering in `src/lower/package.rs`:
//!
//! - a bodyless `@intrinsic` decl lowers to an [`IRFunction`] with
//!   [`FunctionKind::Intrinsic`] and zero basic blocks (the backend
//!   synthesizes the body at emit time)
//! - a call to an `@intrinsic` symbol lowers to an ordinary
//!   [`IRInstruction::Call`] with the same shape as a call to a
//!   regular helper, distinguished only by the callee's `kind` on
//!   the resolved [`IRFunction`]
//! - the seal pass admits the empty-blocks shape for `Intrinsic`
//!   without panicking on the "function has no basic blocks"
//!   contract that gates `Regular` fns
//!
//! Each fixture parses + typechecks + lowers in script-mode so the
//! `@intrinsic fn print(s: String)` decl lives alongside the body
//! that calls it. Project-mode (program) coverage waits on the
//! stdlib-loading slice that imports intrinsics from real `lib/`
//! files.
//!
//! [`IRFunction`]: koja_ir::IRFunction
//! [`FunctionKind`]: koja_ir::FunctionKind

use koja_ir::{FunctionKind, IRInstruction, IRIntrinsicId, IRScript, IRTerminator, IRType};
use koja_parser::ParseMode;

mod common;

use common::{entry_block, lower_script_source_in, mangled_function, typecheck_fail_in};

const PACKAGE: &str = "Global";

fn lower(source: &str) -> IRScript {
    lower_script_source_in(PACKAGE, source)
}

#[test]
fn intrinsic_fn_lowers_to_function_kind_intrinsic_with_empty_blocks() {
    let source = "
        @intrinsic
        fn print(s: String)
        ";

    let script = lower(source);
    let function = mangled_function(&script, &format!("{PACKAGE}.print"));

    let FunctionKind::Intrinsic(id) = &function.kind else {
        panic!(
            "intrinsic fn lowered with wrong kind: expected `Intrinsic(_)`, got {:?}",
            function.kind,
        );
    };
    assert_eq!(
        *id,
        IRIntrinsicId::Print,
        "intrinsic id should map to the `print` variant",
    );
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

    let script = lower(source);
    let mangled = format!("{PACKAGE}.print");
    assert_eq!(script.return_type, IRType::Unit);

    let block = entry_block(&script.blocks);
    let call = block
        .instructions
        .iter()
        .find_map(|inst| match inst {
            IRInstruction::Call { callee, args, .. } => Some((callee, args)),
            _ => None,
        })
        .expect("expected a Call instruction in the script body");
    assert_eq!(call.0.mangled(), mangled);
    assert_eq!(call.1.len(), 1, "print takes exactly one String arg");
    assert_eq!(
        block.terminator,
        IRTerminator::Return { value: None },
        "Unit script discards the intrinsic call result",
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

    let script = lower(source);
    let function = mangled_function(&script, &format!("{PACKAGE}.helper"));
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

    let script = lower(source);
    assert!(
        script.function(&format!("{PACKAGE}.print")).is_some(),
        "intrinsic should be reachable via IRScript::function lookup",
    );
}

/// Mismatched arg types at an intrinsic call site surface through
/// the typecheck pipeline (same path as regular fns), since the IR
/// layer doesn't special-case intrinsics. Pins that intrinsic
/// signatures participate in arg-type checking like any other
/// function.
#[test]
fn intrinsic_arg_type_mismatch_diagnoses_through_typecheck() {
    let source = "
        @intrinsic
        fn print(s: String)

        print(1)
        ";

    let failure = typecheck_fail_in(PACKAGE, source, ParseMode::Script);
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("expects `String`")),
        "expected typecheck arg-mismatch, got: {:?}",
        failure.diagnostics,
    );
}
