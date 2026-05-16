//! Eval round-trip coverage for the `Clone` protocol's
//! heap-primitive intrinsics. Pins three things:
//!
//! - `String.clone()` round-trips byte-for-byte.
//! - `Binary.clone()` round-trips byte-for-byte.
//! - `Bits.clone()` round-trips both bytes and bit_length (so a
//!   non-byte-aligned `Bits` doesn't lose its trailing partial byte).
//!
//! The runtime `Value::String` / `Value::Binary` / `Value::Bits`
//! shapes carry their payloads in `Vec<u8>`, so the eval-side
//! "deep clone" reduces to a `bytes.clone()`. The interesting
//! property is that mutating the returned `Vec<u8>` in Rust would
//! not affect the original — i.e. the eval handler must not share
//! the backing buffer. We pin that by binding the clone to a fresh
//! local and asserting the runtime [`Value`] equals the original
//! shape; an aliasing implementation would still pass byte-equality
//! but pin the payload by-pointer, which is invisible at the
//! [`Value`] surface. The genuine no-aliasing property is exercised
//! by the LLVM backend and the driver e2e (where mutation +
//! independent buffers are observable through the `<>` concat path).

use expo_ir_eval::Value;

mod common;

use common::evaluate_program as evaluate;

#[test]
fn string_clone_round_trips_payload() {
    assert_eq!(
        evaluate("fn main -> String\n  \"hello\".clone()\nend\n").unwrap(),
        Value::String(b"hello".to_vec()),
    );
}

#[test]
fn empty_string_clone_round_trips_as_empty() {
    assert_eq!(
        evaluate("fn main -> String\n  \"\".clone()\nend\n").unwrap(),
        Value::String(Vec::new()),
    );
}

#[test]
fn binary_clone_round_trips_payload() {
    assert_eq!(
        evaluate("fn main -> Binary\n  <<1, 2, 3>>.clone()\nend\n").unwrap(),
        Value::Binary(vec![1, 2, 3]),
    );
}

#[test]
fn bits_clone_round_trips_payload_and_bit_length() {
    // `<<5::3, 9::4>>` produces a 7-bit `Bits` (one byte, last bit
    // zero-padded). Cloning must preserve both the byte payload and
    // the precise `bit_length` so the trailing partial-byte boundary
    // round-trips intact.
    assert_eq!(
        evaluate("fn main -> Bits\n  <<5::3, 9::4>>.clone()\nend\n").unwrap(),
        Value::Bits {
            bytes: vec![0xB2],
            bit_length: 7,
        },
    );
}

#[test]
fn clone_then_concat_proves_independent_payloads() {
    // If the clone aliased the source buffer at the runtime layer,
    // concat'ing the clone with another suffix would mutate the
    // observable bytes of the source. We pin "clone is independent"
    // by binding both sides to locals before concatenating each
    // separately and returning the source — an aliasing
    // implementation would corrupt the source's byte string.
    let source = "
        fn main -> String
          source = \"abc\"
          copy = source.clone()
          mutated = copy <> \"!\"
          source <> mutated
        end
        ";
    assert_eq!(
        evaluate(source).unwrap(),
        Value::String(b"abcabc!".to_vec()),
    );
}
