//! Numeric text parsing shared between backends.
//!
//! The classification rules (what counts as `InvalidFormat` vs
//! `OutOfRange`) back both `Int.parse` and `Float.parse`: the
//! C-ABI helpers in [`crate::string`] map outcomes to return codes
//! for LLVM-emitted IR, and `koja-ir-eval`'s parse shims consume
//! [`ParseOutcome`] directly. This module is the authoritative
//! definition of the return codes; `koja-ir-llvm` mirrors them by
//! spec (see `koja/design/ABI.md` § Numeric parse helpers).

use std::num::IntErrorKind;

/// Return codes for the C-ABI numeric parse helpers
/// (`koja_int_parse` / `koja_float_parse`), mirrored by the LLVM
/// intrinsic emitter (`koja-ir-llvm/src/intrinsics/parse.rs`).
pub const PARSE_INVALID_FORMAT: i64 = 0;
pub const PARSE_OK: i64 = 1;
pub const PARSE_OUT_OF_RANGE: i64 = 2;

/// Outcome of parsing numeric text. Maps 1:1 onto the stdlib's
/// `Result<T, NumericConversionError>` parse returns.
pub enum ParseOutcome<T> {
    InvalidFormat,
    Ok(T),
    OutOfRange,
}

impl<T> ParseOutcome<T> {
    /// The C-ABI return code for this outcome.
    pub fn code(&self) -> i64 {
        match self {
            Self::InvalidFormat => PARSE_INVALID_FORMAT,
            Self::Ok(_) => PARSE_OK,
            Self::OutOfRange => PARSE_OUT_OF_RANGE,
        }
    }

    /// Map the success payload, preserving the failure arms.
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> ParseOutcome<U> {
        match self {
            Self::InvalidFormat => ParseOutcome::InvalidFormat,
            Self::Ok(v) => ParseOutcome::Ok(f(v)),
            Self::OutOfRange => ParseOutcome::OutOfRange,
        }
    }
}

/// Parse `text` (already trimmed) as a 64-bit signed integer.
/// Well-formed integers that overflow 64 bits are out of range.
pub fn parse_int_text(text: &str) -> ParseOutcome<i64> {
    match text.parse::<i64>() {
        Ok(v) => ParseOutcome::Ok(v),
        Err(err)
            if matches!(
                err.kind(),
                IntErrorKind::NegOverflow | IntErrorKind::PosOverflow
            ) =>
        {
            ParseOutcome::OutOfRange
        }
        Err(_) => ParseOutcome::InvalidFormat,
    }
}

/// Parse `text` (already trimmed) as a 64-bit float. Only finite
/// decimal text parses: a well-formed magnitude that overflows
/// `f64` (e.g. `1e999`) is out of range, while the `inf` /
/// `infinity` / `nan` tokens Rust's parser accepts are rejected as
/// invalid — Koja has no literal syntax for them.
pub fn parse_float_text(text: &str) -> ParseOutcome<f64> {
    match text.parse::<f64>() {
        Ok(v) if v.is_finite() => ParseOutcome::Ok(v),
        Ok(_)
            if text
                .chars()
                .any(|c| c.is_ascii_alphabetic() && !matches!(c, 'e' | 'E')) =>
        {
            ParseOutcome::InvalidFormat
        }
        Ok(_) => ParseOutcome::OutOfRange,
        Err(_) => ParseOutcome::InvalidFormat,
    }
}
