//! Typecheck coverage for `<<segments>>` binary patterns
//! ([`Pattern::Binary`]).
//!
//! Pairs with the binary literal coverage in
//! `resolve_binary_literal.rs`: the resolve layer shares its
//! per-segment width arithmetic with the literal side, but the
//! pattern side is responsible for registering bindings into the
//! arm scope and rejecting shapes that don't lower cleanly
//! (dynamic-width, byte-unit, float-extract, non-byte-aligned
//! `: Binary` rests).

use koja_ast::util::dedent;

mod common;

use common::{typecheck_script as typecheck, typecheck_script_fail as typecheck_fail};

#[test]
fn literal_only_pattern_typechecks() {
    typecheck(&dedent(
        "
        data = <<0xFF, 0x01, 0x02>>
          match data
            <<0xFF, _::8, _::8>> -> 1
            _ -> 0
          end
        ",
    ));
}

#[test]
fn binding_only_pattern_typechecks() {
    typecheck(&dedent(
        "
        data = <<0xFF, 0x01, 0x02>>
          match data
            <<tag::8, val::16>> -> tag + val
            _ -> 0
          end
        ",
    ));
}

#[test]
fn greedy_tail_binary_typechecks() {
    typecheck(&dedent(
        "
        data = <<0xFF, 0x01, 0x02>>
          match data
            <<head::8, rest: Binary>> -> head
            _ -> 0
          end
        ",
    ));
}

#[test]
fn greedy_tail_bits_typechecks() {
    typecheck(&dedent(
        "
        data = <<0xFF, 0x01, 0x02>>
          match data
            <<head::8, rest: Bits>> -> head
            _ -> 0
          end
        ",
    ));
}

#[test]
fn typed_binding_pattern_typechecks() {
    typecheck(&dedent(
        "
        data = <<0xFF, 0x01, 0x02>>
          match data
            <<tag: UInt8, len: UInt16>> -> 1
            _ -> 0
          end
        ",
    ));
}

#[test]
fn string_literal_segment_typechecks() {
    typecheck(&dedent(
        "
        data = <<\"GET hi\">>
          match data
            <<\"GET \", rest: Binary>> -> 1
            _ -> 0
          end
        ",
    ));
}

#[test]
fn non_binary_subject_diagnoses() {
    let failure = typecheck_fail(&dedent(
        "
        x = 1
        match x
            <<tag::8>> -> 1
            _ -> 0
          end
        ",
    ));
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("requires `Binary` or `Bits` subject")),
        "expected non-binary-subject diagnostic, got {:?}",
        failure.diagnostics,
    );
}

#[test]
fn dynamic_width_pattern_diagnoses() {
    let failure = typecheck_fail(&dedent(
        "
        data = <<0xFF>>
          n = 8
          match data
            <<x::n>> -> x
            _ -> 0
          end
        ",
    ));
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("dynamic-width binary pattern")),
        "expected dynamic-width diagnostic, got {:?}",
        failure.diagnostics,
    );
}

#[test]
fn byte_unit_pattern_diagnoses() {
    let failure = typecheck_fail(&dedent(
        "
        data = <<0xFF, 0x01, 0x02, 0x03>>
          match data
            <<x::2 byte>> -> x
            _ -> 0
          end
        ",
    ));
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("`::N byte`")),
        "expected byte-unit diagnostic, got {:?}",
        failure.diagnostics,
    );
}

#[test]
fn float_extract_diagnoses() {
    let failure = typecheck_fail(&dedent(
        "
        data = <<1.0: Float32>>
          match data
            <<x: Float32>> -> 1
            _ -> 0
          end
        ",
    ));
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("float-extract binary pattern")),
        "expected float-extract diagnostic, got {:?}",
        failure.diagnostics,
    );
}

#[test]
fn unaligned_binary_tail_diagnoses() {
    // `tag::3` + `rest: Binary` has a 3-bit fixed prefix → not
    // byte-aligned → reject.
    let failure = typecheck_fail(&dedent(
        "
        data = <<0::3, 0::5>>
          match data
            <<tag::3, rest: Binary>> -> tag
            _ -> 0
          end
        ",
    ));
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("byte-aligned prefix")),
        "expected byte-aligned-prefix diagnostic, got {:?}",
        failure.diagnostics,
    );
}

#[test]
fn literal_overflow_diagnoses() {
    let failure = typecheck_fail(&dedent(
        "
        data = <<0xFF>>
          match data
            <<300::8>> -> 1
            _ -> 0
          end
        ",
    ));
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("does not fit in 8 unsigned bits")),
        "expected literal-overflow diagnostic, got {:?}",
        failure.diagnostics,
    );
}

#[test]
fn missing_catch_all_diagnoses() {
    let failure = typecheck_fail(&dedent(
        "
        data = <<0xFF>>
          match data
            <<tag::8>> -> tag
          end
        ",
    ));
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("wildcard `_` or binding catch-all")),
        "expected missing-catch-all diagnostic, got {:?}",
        failure.diagnostics,
    );
}
