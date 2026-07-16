//! Typecheck coverage for `<<segments>>` binary literals
//! ([`ExprKind::BinaryLiteral`]).
//!
//! Per-segment validation, byte-aligned vs sub-byte total -> Binary
//! / Bits, and feature-gap diagnostics for dynamic widths or
//! incompatible value/modifier pairs. Pairs with the lowering
//! coverage in `koja-ir/tests/lower_binary_literal.rs` (which
//! pins the IR shape, `IRInstruction::BinaryConstruct` with the
//! per-segment `bit_offset` accumulator) and the eval coverage in
//! `koja-ir-eval/tests/binary_literal.rs` (which pins the
//! byte-for-byte runtime layout).

mod common;

use common::{
    assert_script_fails_with, global_leaf, trailing_resolution, typecheck_script as typecheck,
};

#[test]
fn byte_aligned_literal_resolves_to_binary() {
    // `<<1, 2, 3>>`: three default-8-bit integer segments -> 24
    // bits -> byte-aligned -> Binary.
    let checked = typecheck("<<1, 2, 3>>\n");
    assert_eq!(
        trailing_resolution(&checked),
        global_leaf(&checked, "Binary")
    );
}

#[test]
fn sized_integer_segment_resolves_to_binary() {
    // `<<255::16>>`: one 16-bit integer segment -> 16 bits -> Binary.
    let checked = typecheck("<<255::16>>\n");
    assert_eq!(
        trailing_resolution(&checked),
        global_leaf(&checked, "Binary")
    );
}

#[test]
fn type_annotated_integer_segment_resolves_to_binary() {
    // `<<x: Int16>>`: one Int16 segment -> 16 bits -> Binary.
    let checked = typecheck("<<42: Int16>>\n");
    assert_eq!(
        trailing_resolution(&checked),
        global_leaf(&checked, "Binary")
    );
}

#[test]
fn float32_segment_resolves_to_binary() {
    // `<<1.0: Float32>>`: 32 bits -> Binary.
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
    // `<<"hi">>`: 16 bits -> Binary.
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
    // Empty `<<>>` is 0 bits, vacuously byte-aligned -> Binary.
    let checked = typecheck("<<>>\n");
    assert_eq!(
        trailing_resolution(&checked),
        global_leaf(&checked, "Binary")
    );
}

#[test]
fn dynamic_width_segment_diagnoses() {
    // `n` is a runtime int, so `<<x::n>>` is feature-gapped.
    assert_script_fails_with("n = 8\n  x = 1\n  <<x::n>>\n", &["dynamic-width"]);
}

#[test]
fn float_with_size_modifier_diagnoses() {
    // `::N` only applies to integer values. Typing `1.0::32` is a
    // misuse that should be rejected loudly.
    assert_script_fails_with("<<1.0::32>>\n", &["`Int`-typed value"]);
}

#[test]
fn unknown_type_annotation_diagnoses() {
    // `Bool` is a recognized stdlib stub but not a valid binary
    // segment type annotation.
    assert_script_fails_with("<<true: Bool>>\n", &["not a recognized primitive"]);
}

#[test]
fn string_segment_with_size_diagnoses() {
    // String segments don't carry `::N`. Combining the two is a
    // misuse the language layer should reject before lower.
    assert_script_fails_with("<<\"hi\"::8>>\n", &["`String`-valued binary segment"]);
}

#[test]
fn byte_unit_size_resolves_to_binary() {
    // `<<x::4 byte>>` = 32 bits -> Binary.
    let checked = typecheck("<<7::4 byte>>\n");
    assert_eq!(
        trailing_resolution(&checked),
        global_leaf(&checked, "Binary")
    );
}

#[test]
fn bare_binary_value_splices_and_resolves_to_binary() {
    // A bare segment classifies by its value's type, so a
    // `Binary`-typed value splices without a `: Binary` tag.
    let checked = typecheck("b = <<1, 2>>\n  <<0x51, b>>\n");
    assert_eq!(
        trailing_resolution(&checked),
        global_leaf(&checked, "Binary")
    );
}

#[test]
fn tagged_splice_resolves_to_binary() {
    let checked = typecheck("b = <<1>>\n  <<b: Binary, 0x02>>\n");
    assert_eq!(
        trailing_resolution(&checked),
        global_leaf(&checked, "Binary")
    );
}

#[test]
fn binary_tag_on_non_binary_value_diagnoses() {
    assert_script_fails_with(
        "n = 5\n  <<n: Binary>>\n",
        &["requires a `Binary`-typed value"],
    );
}

#[test]
fn bare_segment_of_inadmissible_type_diagnoses() {
    // A bare segment admits `Int` (8-bit) and `Binary` (splice).
    // Anything else needs an explicit modifier.
    assert_script_fails_with("<<1.5>>\n", &["or a `Binary`-typed value"]);
}

#[test]
fn unaligned_run_before_splice_diagnoses() {
    // Each fixed-width run around a splice becomes its own Binary,
    // so a sub-byte run cannot precede a splice.
    assert_script_fails_with("b = <<1>>\n  <<5::3, b>>\n", &["must total whole bytes"]);
}

#[test]
fn unaligned_run_after_splice_diagnoses() {
    assert_script_fails_with("b = <<1>>\n  <<b, 5::3>>\n", &["must total whole bytes"]);
}

#[test]
fn string_splice_tag_diagnoses() {
    // Only `Binary` values splice. `: String` gets a pointed
    // diagnostic instead of the unrecognized-primitive list.
    assert_script_fails_with("s = \"hi\"\n  <<s: String>>\n", &["cannot be spliced"]);
}
