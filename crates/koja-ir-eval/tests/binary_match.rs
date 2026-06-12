//! Behavioral coverage for `IRInstruction::BinaryMatch` under the
//! interpreter: length gating, literal int/byte tests, sign-aware
//! and endian-aware bindings, discards, and greedy `Binary`/`Bits`
//! tails. Counterpart to `koja-ir-llvm/tests/binary_match.rs`,
//! which pins the same shapes as IR-text snapshots.

mod common;

use common::evaluate_script;
use koja_ast::util::dedent;
use koja_ir_eval::Value;

fn evaluate(source: &str) -> Value {
    evaluate_script(&dedent(source)).expect("script should evaluate")
}

#[test]
fn literal_bytes_match_selects_the_arm() {
    let source = r#"
        data = <<0x01, 0x02, 0x03>>
        match data
          <<0x01, 0x02, 0x03>> -> 1
          _ -> 0
        end
        "#;
    assert_eq!(evaluate(source), Value::Int(1));
}

#[test]
fn literal_mismatch_falls_through() {
    let source = r#"
        data = <<0x01, 0x02, 0x03>>
        match data
          <<0x01, 0x02, 0x04>> -> 1
          _ -> 0
        end
        "#;
    assert_eq!(evaluate(source), Value::Int(0));
}

#[test]
fn length_mismatch_falls_through() {
    let source = r#"
        data = <<0xAA, 0xBB, 0xCC>>
        match data
          <<a::8, b::8>> -> a + b
          _ -> -1
        end
        "#;
    assert_eq!(evaluate(source), Value::Int(-1));
}

#[test]
fn string_literal_segment_matches_byte_run() {
    let source = r#"
        data = <<0x48, 0x49>>
        match data
          <<"HI">> -> 1
          _ -> 0
        end
        "#;
    assert_eq!(evaluate(source), Value::Int(1));
}

#[test]
fn unsigned_binding_zero_extends() {
    let source = r#"
        data = <<0xAB>>
        match data
          <<v::8>> -> v
          _ -> 0
        end
        "#;
    assert_eq!(evaluate(source), Value::Int(0xAB));
}

#[test]
fn signed_binding_sign_extends() {
    let source = r#"
        neg = <<0xFF>>
        match neg
          <<v::8 signed>> -> v
          _ -> 0
        end
        "#;
    assert_eq!(evaluate(source), Value::Int(-1));
}

#[test]
fn little_endian_binding_shuffles_bytes() {
    let source = r#"
        data = <<0x01, 0x02>>
        match data
          <<v::16 little>> -> v
          _ -> 0
        end
        "#;
    assert_eq!(evaluate(source), Value::Int(0x0201));
}

#[test]
fn discard_segment_keeps_offsets_aligned() {
    let source = r#"
        data = <<0xAA, 0xBB, 0xCC>>
        match data
          <<_::8, v::8, _::8>> -> v
          _ -> 0
        end
        "#;
    assert_eq!(evaluate(source), Value::Int(0xBB));
}

#[test]
fn greedy_binary_tail_binds_remaining_bytes() {
    let source = r#"
        stream = <<0xAA, 0xBB, 0xCC, 0xDD>>
        match stream
          <<_head::8, rest: Binary>> -> rest
          _ -> <<>>
        end
        "#;
    assert_eq!(evaluate(source), Value::binary(vec![0xBB, 0xCC, 0xDD]));
}

#[test]
fn greedy_tail_accepts_longer_subjects() {
    let source = r#"
        stream = <<0xAA, 0xBB, 0xCC, 0xDD>>
        match stream
          <<head::8, _rest: Binary>> -> head
          _ -> 0
        end
        "#;
    assert_eq!(evaluate(source), Value::Int(0xAA));
}

#[test]
fn sub_byte_bindings_split_a_byte() {
    let source = r#"
        bits = <<0b10110100::8>>
        match bits
          <<hi::3, lo::5>> -> hi * 100 + lo
          _ -> -1
        end
        "#;
    // hi = 0b101 = 5, lo = 0b10100 = 20.
    assert_eq!(evaluate(source), Value::Int(520));
}

#[test]
fn multi_segment_postgres_style_frame() {
    // The shortener's Postgres driver shape: a 1-byte tag, a 32-bit
    // big-endian length, and a greedy payload tail.
    let source = r#"
        frame = <<0x52, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00>>
        match frame
          <<tag::8, length::32, _payload: Binary>> -> tag * 1000 + length
          _ -> -1
        end
        "#;
    assert_eq!(evaluate(source), Value::Int(0x52 * 1000 + 8));
}
