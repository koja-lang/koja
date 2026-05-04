//! Feature-gap diagnostics surfaced by the alpha IR lowering phase.
//!
//! These tests pin the contract that unsupported shapes (Float
//! literals, assignment statements, extern fn bodies) bubble up
//! through `lower_program` as [`LowerError::Diagnostics`] *instead
//! of panicking*. Compiler-bug panics (seal invariant violations)
//! still crash hard; that invariant is covered by `seal.rs`'s
//! internal asserts.
//!
//! String literals aren't exercised here because the parser routes
//! `"..."` through `ExprKind::String` (interpolation-aware) and
//! typecheck rejects that expression kind before lowering ever sees
//! it; the `Literal::String` arm in `lower_literal` is defensive.
//!
//! Each test drives `parse_program → check_program → lower_program` to
//! keep the failure path faithful to the driver's pipeline.

use std::path::PathBuf;

use expo_alpha_ir::{LowerError, lower_program};
use expo_alpha_typecheck::check_program;
use expo_ast::identifier::Identifier;
use expo_parser::{ParseMode, SourceFile, parse_program};

const PACKAGE: &str = "TestApp";

fn lower_err(source: &str, entry: &str) -> LowerError {
    let parsed = parse_program(
        vec![SourceFile {
            package: PACKAGE.to_string(),
            path: PathBuf::from("lower_failures.expo"),
            source: source.to_string(),
        }],
        ParseMode::File,
    );
    let checked =
        check_program(parsed).unwrap_or_else(|f| panic!("alpha typecheck must succeed here:\n{f}"));
    let entry_id = Identifier::new(PACKAGE, vec![entry.to_string()]);
    lower_program(&checked, entry_id).expect_err("lowering should surface diagnostics")
}

fn expect_diagnostics(err: LowerError) -> Vec<String> {
    match err {
        LowerError::Diagnostics(d) => d.into_iter().map(|diag| diag.message).collect(),
        other => panic!("expected Diagnostics, got {other:?}"),
    }
}

#[test]
fn float_literal_in_body_surfaces_feature_gap_diagnostic() {
    let err = lower_err("fn main\n  1.5\nend\n", "main");
    let messages = expect_diagnostics(err);
    assert_eq!(messages.len(), 1);
    assert!(
        messages[0].contains("Float literals"),
        "expected Float-literal diagnostic, got: {messages:?}",
    );
}

#[test]
fn assignment_statement_surfaces_feature_gap_diagnostic() {
    let err = lower_err("fn main\n  x = 1\nend\n", "main");
    let messages = expect_diagnostics(err);
    assert_eq!(messages.len(), 1);
    assert!(
        messages[0].contains("assignment statements"),
        "expected assignment diagnostic, got: {messages:?}",
    );
}

#[test]
fn extern_fn_without_body_surfaces_feature_gap_diagnostic() {
    let source = "@extern \"C\"\nfn missing() -> Int\n";
    let err = lower_err(source, "missing");
    let messages = expect_diagnostics(err);
    assert_eq!(messages.len(), 1);
    assert!(
        messages[0].contains("extern fn `missing`"),
        "expected extern-fn diagnostic, got: {messages:?}",
    );
}

/// When one function fails to lower, other functions in the same
/// package still get walked — the failing one is simply omitted, and
/// the final [`LowerError::Diagnostics`] carries *only* the diagnostic
/// from the function that actually failed. Pins the per-function
/// fail-fast contract so a single bad function doesn't mask issues in
/// other ones and doesn't spew spurious errors either.
#[test]
fn partial_failure_reports_only_the_failing_function_diagnostic() {
    let source = "fn main\n  1\nend\nfn broken\n  1.5\nend\n";
    let err = lower_err(source, "main");
    let messages = expect_diagnostics(err);
    assert_eq!(
        messages.len(),
        1,
        "expected a single diagnostic from the failing fn, got: {messages:?}",
    );
    assert!(
        messages[0].contains("Float literals"),
        "expected Float-literal diagnostic from `broken`, got: {messages:?}",
    );
}

/// Multiple feature gaps inside a single function should emit *one*
/// diagnostic — the first one seen — and abort walking that function.
/// Pins the fail-fast-per-function contract explicitly: here the Float
/// literal trips first; if lowering kept walking it would also trip on
/// the assignment and produce two diagnostics instead of one.
#[test]
fn fail_fast_within_function_emits_single_diagnostic() {
    let source = "fn main\n  1.5\n  x = 2\nend\n";
    let err = lower_err(source, "main");
    let messages = expect_diagnostics(err);
    assert_eq!(
        messages.len(),
        1,
        "expected fail-fast within a function, got: {messages:?}",
    );
    assert!(
        messages[0].contains("Float literals"),
        "expected first diagnostic to be Float literal, got: {messages:?}",
    );
}
