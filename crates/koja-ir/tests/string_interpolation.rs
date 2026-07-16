//! Lowering coverage for interpolated string literals:
//! `"prefix #{x.format()} suffix"` desugars at IR-lower time into a
//! chain of N-1 `IRInstruction::Concat { kind: String }` instructions
//! over the N parts. Each literal part lowers to a
//! `ConstValue::String`. Each interpolation part recurses through
//! [`lower_expr`] (the synthesizer wraps every interpolation in
//! `.format()` so the inner expr is already `String`-typed by the
//! time we see it).
//!
//! This file pins the part-count -> concat-count contract (N parts
//! -> N-1 concats) and the concat-kind invariant (always
//! `ConcatKind::String` regardless of how the part was sourced).

use koja_ir::{ConcatKind, ConstValue, IRBasicBlock, IRInstruction, IRType};

mod common;

use common::{all_instructions, lower_script_source as lower};

fn count_string_concats(blocks: &[IRBasicBlock]) -> usize {
    all_instructions(blocks)
        .filter(|inst| {
            matches!(
                inst,
                IRInstruction::Concat {
                    kind: ConcatKind::String,
                    ..
                },
            )
        })
        .count()
}

fn count_string_consts(blocks: &[IRBasicBlock]) -> usize {
    all_instructions(blocks)
        .filter(|inst| {
            matches!(
                inst,
                IRInstruction::Const {
                    value: ConstValue::String(_),
                    ..
                },
            )
        })
        .count()
}

#[test]
fn three_part_interpolation_emits_two_string_concats() {
    // `"a=#{x.format()}b"` has 3 parts (Literal "a=", Interp,
    // Literal "b") so the lowerer threads two binary concats:
    // `(("a=" <> x.format()) <> "b")`.
    let source = "
        x = 1
        \"a=#{x.format()}b\"
        ";

    let script = lower(source);
    assert_eq!(script.return_type, IRType::String);
    assert_eq!(
        count_string_concats(&script.blocks),
        2,
        "expected N-1 = 2 string concats for 3 string parts",
    );
}

#[test]
fn five_part_interpolation_emits_four_string_concats() {
    // Two interleaved interpolations between three literal segments
    // -> 5 parts -> 4 concats.
    let source = "
        x = 1
        y = 2
        \"x=#{x.format()} y=#{y.format()}.\"
        ";

    let script = lower(source);
    assert_eq!(count_string_concats(&script.blocks), 4);
}

#[test]
fn lone_interpolation_emits_no_concat_just_the_inner_value() {
    // A single-part interpolation has nothing to fold. The
    // `format()` call's `String` value flows straight through to the
    // function return.
    let source = "
        x = 1
        \"#{x.format()}\"
        ";

    let script = lower(source);
    assert_eq!(
        count_string_concats(&script.blocks),
        0,
        "single-part interpolation should not emit a Concat",
    );
}

#[test]
fn lone_literal_emits_one_const_no_concat() {
    // Sanity check that the concat-chain shape doesn't kick in for
    // plain (non-interpolated) string literals. The existing
    // `lower_string` fast path stays intact.
    let source = "
        \"hello\"
        ";

    let script = lower(source);
    assert_eq!(count_string_concats(&script.blocks), 0);
    assert_eq!(count_string_consts(&script.blocks), 1);
}

#[test]
fn literal_then_interpolation_then_literal_concats_are_string_kinded() {
    // ConcatKind::String is the only kind interpolation ever emits.
    // Pin the kind explicitly so a later refactor toward `Binary` /
    // `Bits` interpolation surfaces as a test failure rather than
    // a runtime mismatch.
    let source = "
        x = 1
        \"prefix-#{x.format()}-suffix\"
        ";

    let script = lower(source);
    let kinds: Vec<ConcatKind> = all_instructions(&script.blocks)
        .filter_map(|inst| match inst {
            IRInstruction::Concat { kind, .. } => Some(*kind),
            _ => None,
        })
        .collect();
    assert!(
        kinds.iter().all(|k| matches!(k, ConcatKind::String)),
        "every Concat should be string-kinded; got {kinds:?}",
    );
    assert_eq!(kinds.len(), 2);
}
