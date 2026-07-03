//! Typecheck coverage for `<<segments>>` binary literals
//! ([`ExprKind::BinaryLiteral`]).
//!
//! Per-segment validation, byte-aligned vs sub-byte total → Binary
//! / Bits, and feature-gap diagnostics for dynamic widths or
//! incompatible value/modifier pairs. Pairs with the lowering
//! coverage in `koja-ir/tests/lower_binary_literal.rs` (which
//! pins the IR shape, `IRInstruction::BinaryConstruct` with the
//! per-segment `bit_offset` accumulator) and the eval coverage in
//! `koja-ir-eval/tests/binary_literal.rs` (which pins the
//! byte-for-byte runtime layout).

use koja_ast::ast::Statement;
use koja_ast::identifier::{Identifier, Resolution, ResolvedType};
use koja_typecheck::CheckedProgram;

mod common;

use common::{PACKAGE, typecheck_script as typecheck, typecheck_script_fail as typecheck_fail};

fn trailing_resolution(checked: &CheckedProgram) -> ResolvedType {
    let pkg = checked
        .packages
        .iter()
        .find(|p| p.package == PACKAGE)
        .expect("checked program is missing the test package");
    let file = pkg.files.first().expect("package has no files");
    let body = file
        .body
        .as_deref()
        .expect("script-mode file must keep statements on File.body");
    let trailing = body.last().expect("expected at least one statement");
    match trailing {
        Statement::Expr(expr) => expr.resolution.clone(),
        other => panic!("expected Statement::Expr as trailing statement, got {other:?}"),
    }
}

fn global_leaf(checked: &CheckedProgram, name: &str) -> ResolvedType {
    let ident = Identifier::new("Global", vec![name.to_string()]);
    let (id, _) = checked
        .registry
        .lookup(&ident)
        .unwrap_or_else(|| panic!("stdlib stub `Global.{name}` missing from registry"));
    ResolvedType::leaf(Resolution::Global(id))
}

#[test]
fn byte_aligned_literal_resolves_to_binary() {
    // `<<1, 2, 3>>`: three default-8-bit integer segments → 24
    // bits → byte-aligned → Binary.
    let checked = typecheck("<<1, 2, 3>>\n");
    assert_eq!(
        trailing_resolution(&checked),
        global_leaf(&checked, "Binary")
    );
}

#[test]
fn sized_integer_segment_resolves_to_binary() {
    // `<<255::16>>`: one 16-bit integer segment → 16 bits → Binary.
    let checked = typecheck("<<255::16>>\n");
    assert_eq!(
        trailing_resolution(&checked),
        global_leaf(&checked, "Binary")
    );
}

#[test]
fn type_annotated_integer_segment_resolves_to_binary() {
    // `<<x: Int16>>`: one Int16 segment → 16 bits → Binary.
    let checked = typecheck("<<42: Int16>>\n");
    assert_eq!(
        trailing_resolution(&checked),
        global_leaf(&checked, "Binary")
    );
}

#[test]
fn float32_segment_resolves_to_binary() {
    // `<<1.0: Float32>>`: 32 bits → Binary.
    let checked = typecheck("<<1.0: Float32>>\n");
    assert_eq!(
        trailing_resolution(&checked),
        global_leaf(&checked, "Binary")
    );
}

#[test]
fn float64_segment_resolves_to_binary() {
    let checked = typecheck("<<2.5: Float64>>\n");
    assert_eq!(
        trailing_resolution(&checked),
        global_leaf(&checked, "Binary")
    );
}

#[test]
fn string_segment_resolves_to_binary() {
    // `<<"hi">>`: 16 bits → Binary.
    let checked = typecheck("<<\"hi\">>\n");
    assert_eq!(
        trailing_resolution(&checked),
        global_leaf(&checked, "Binary")
    );
}

#[test]
fn sub_byte_literal_resolves_to_bits() {
    // `<<1::3, 2::5>>`: 3 + 5 = 8 bits, byte-aligned, so Binary.
    // To get Bits we need a non-multiple-of-8 total: 3 + 4 = 7.
    let checked = typecheck("<<1::3, 2::4>>\n");
    assert_eq!(trailing_resolution(&checked), global_leaf(&checked, "Bits"));
}

#[test]
fn empty_literal_resolves_to_binary() {
    // Empty `<<>>` is 0 bits, vacuously byte-aligned → Binary.
    let checked = typecheck("<<>>\n");
    assert_eq!(
        trailing_resolution(&checked),
        global_leaf(&checked, "Binary")
    );
}

#[test]
fn dynamic_width_segment_diagnoses() {
    // `n` is a runtime int, so `<<x::n>>` is feature-gapped.
    let failure = typecheck_fail("n = 8\n  x = 1\n  <<x::n>>\n");
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("dynamic-width")),
        "expected dynamic-width diagnostic, got {:?}",
        failure.diagnostics,
    );
}

#[test]
fn float_with_size_modifier_diagnoses() {
    // `::N` only applies to integer values. Typing `1.0::32` is a
    // misuse that should be rejected loudly.
    let failure = typecheck_fail("<<1.0::32>>\n");
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("`Int`-typed value")),
        "expected `::N` requires Int diagnostic, got {:?}",
        failure.diagnostics,
    );
}

#[test]
fn unknown_type_annotation_diagnoses() {
    // `Bool` is a recognized stdlib stub but not a valid binary
    // segment type annotation.
    let failure = typecheck_fail("<<true: Bool>>\n");
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("not a recognized primitive")),
        "expected unrecognized-type diagnostic, got {:?}",
        failure.diagnostics,
    );
}

#[test]
fn string_segment_with_size_diagnoses() {
    // String segments don't carry `::N`. Combining the two is a
    // misuse the language layer should reject before lower.
    let failure = typecheck_fail("<<\"hi\"::8>>\n");
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("`String`-valued binary segment")),
        "expected string-with-size diagnostic, got {:?}",
        failure.diagnostics,
    );
}

#[test]
fn byte_unit_size_resolves_to_binary() {
    // `<<x::4 byte>>` = 32 bits → Binary.
    let checked = typecheck("<<7::4 byte>>\n");
    assert_eq!(
        trailing_resolution(&checked),
        global_leaf(&checked, "Binary")
    );
}
