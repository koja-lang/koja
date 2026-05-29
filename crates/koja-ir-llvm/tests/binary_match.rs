//! IR-text snapshot tests for `IRInstruction::BinaryMatch` lowering
//! ([`crate::emit::binary_match::emit_binary_match`]). Pairs with
//! `src/emit/binary_match.rs` in source.
//!
//! Three shapes are pinned, one per branch of the per-segment
//! dispatch:
//!
//! - Literal-bytes arm — touches the `memcmp` extern declaration
//!   and a `lit_ptr` private constant for the byte payload.
//! - Sign-extended binding — proves the `signed` modifier emits an
//!   `ashr` after the `shl`, fixing the v1 latent bug where
//!   `signed` was a no-op.
//! - Greedy `: Binary` tail — proves the malloc/memcpy block that
//!   stamps the bit-length header for the new payload.

use koja_ast::util::dedent;
use koja_ir_llvm::emit_llvm_ir;

mod common;

use common::{APP_NAME, assert_contains, assert_main_shape, lower_program_source as lower};

#[test]
fn binary_match_int_literal_emits_byte_extract_and_eq() {
    // Byte-literal patterns walk the byte-shift extract loop and
    // emit per-segment `icmp eq` against the masked extracted bits.
    // The length check uses `icmp eq` (no greedy tail) and pins the
    // running `bin_pat_byte_len` SSA name from
    // `emit_binary_match::shift_right_by_three`.
    let source = "
        fn main
          data = <<0x01, 0x02, 0x03>>
          match data
            <<0x01, 0x02, 0x03>> -> 1
            _ -> 0
          end
        end
    ";

    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "bin_pat_bit_len");
    assert_contains(&ir_text, "bin_pat_len_ok");
    assert_contains(&ir_text, "icmp eq");
}

#[test]
fn binary_match_string_literal_segment_emits_memcmp_extern() {
    // The `<<"HI">>` arm routes through the `LiteralBytes` segment
    // shape, which `memcmp`s the byte run against a private constant
    // payload. Pins the extern declaration so a regression that
    // drops the bytes path surfaces as a compile-time miss.
    let source = "
        fn main
          data = <<0x48, 0x49>>
          match data
            <<\"HI\">> -> 1
            _ -> 0
          end
        end
    ";

    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "@memcmp(ptr, ptr, i64)");
}

#[test]
fn binary_match_signed_binding_emits_sign_extend_shl_ashr() {
    // The `signed` modifier must lower to `shl` + arithmetic `ashr`
    // (v1 always emitted an unsigned extraction and discarded the
    // modifier — the pipeline fixes that by routing `BinarySign::Signed`
    // through `extend_for_sign`).
    let source = "
        fn main
          neg = <<0xFF>>
          match neg
            <<v::8 signed>> -> v
            _ -> 0
          end
        end
    ";

    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "sign_shl");
    assert_contains(&ir_text, "sign_ashr");
}

#[test]
fn binary_match_unsigned_binding_skips_sign_extend() {
    // The flip side: an unsigned byte binding must not emit the
    // `sign_*` IR (otherwise the v1 fix would have over-rotated and
    // changed unsigned semantics).
    let source = "
        fn main
          data = <<0xAB>>
          match data
            <<v::8>> -> v
            _ -> 0
          end
        end
    ";

    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    assert!(
        !ir_text.contains("sign_shl"),
        "unsigned binary binding should not emit sign-extend IR; got:\n{ir_text}",
    );
}

#[test]
fn binary_match_greedy_tail_emits_malloc_and_memcpy() {
    // Greedy `: Binary` allocates a fresh `[i64 bit_length][payload]`
    // block and `memcpy`s the remaining bytes into it. Pins the
    // tail-alloc-size add and the memcpy call.
    let source = "
        fn main
          stream = <<0xAA, 0xBB, 0xCC, 0xDD>>
          match stream
            <<head::8, rest: Binary>> -> head
            _ -> 0
          end
        end
    ";

    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "tail_alloc_size");
    assert_contains(&ir_text, "tail_payload");
    assert_contains(&ir_text, "tail_cpy");
    assert_contains(&ir_text, "declare ptr @koja_alloc(i64)");
}

#[test]
fn binary_match_uge_length_check_for_greedy_tail() {
    // Greedy tails must use unsigned-greater-or-equal for the length
    // check; without a greedy tail it's equality.
    let source = "
        fn main
          stream = <<0xAA, 0xBB, 0xCC, 0xDD>>
          match stream
            <<head::8, rest: Binary>> -> head
            _ -> 0
          end
        end
    ";

    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    // `icmp uge` for the greedy-tail length check.
    assert_contains(&ir_text, "icmp uge");
}
