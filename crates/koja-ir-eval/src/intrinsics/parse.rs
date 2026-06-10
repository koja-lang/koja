//! `Int.parse(input: String) -> Result<Int, NumericConversionError>`
//! and `Float.parse(input: String) -> Result<Float, NumericConversionError>`.
//!
//! Both trim leading / trailing whitespace and hand the rest to
//! [`koja_runtime::parse_text`] — the same classification the LLVM
//! backend's runtime helpers use, so the two backends can't drift
//! on what counts as `InvalidFormat` vs `OutOfRange` (a well-formed
//! number that doesn't fit: an overflowing integer, or a float
//! magnitude that rounds to infinity). The Result enum's symbol
//! comes from `function.return_type`; the error variant tag is
//! resolved by name via `helpers::conversion_error_value`.

use koja_ir::{IRFunction, ParseTarget};
use koja_runtime::parse_text::{ParseOutcome, parse_float_text, parse_int_text};

use crate::error::RuntimeError;
use crate::interpreter::CallResolver;
use crate::intrinsics::helpers;
use crate::value::Value;

pub(super) fn dispatch<R: CallResolver>(
    target: ParseTarget,
    function: &IRFunction,
    args: &[Value],
    resolver: &R,
) -> Result<Value, RuntimeError> {
    let bytes = match args {
        [Value::String(bytes)] => bytes.as_slice(),
        _ => {
            return Err(RuntimeError::TypeMismatch {
                detail: format!("{target:?}.parse expects a single String argument; got {args:?}"),
            });
        }
    };
    let result_symbol = helpers::enum_return_symbol(function, &format!("{target:?}.parse"))?;
    let outcome = match std::str::from_utf8(bytes) {
        Ok(s) => {
            let trimmed = s.trim();
            match target {
                ParseTarget::Int => parse_int_text(trimmed).map(Value::Int),
                ParseTarget::Float => parse_float_text(trimmed).map(Value::Float64),
            }
        }
        // A Koja `String` is valid UTF-8 by construction; treat a
        // malformed payload as unparseable rather than erroring.
        Err(_) => ParseOutcome::InvalidFormat,
    };
    let parsed = match outcome {
        ParseOutcome::Ok(v) => Ok(v),
        ParseOutcome::InvalidFormat => Err(helpers::conversion_error_value(
            &result_symbol,
            resolver,
            "InvalidFormat",
        )?),
        ParseOutcome::OutOfRange => Err(helpers::conversion_error_value(
            &result_symbol,
            resolver,
            "OutOfRange",
        )?),
    };
    Ok(helpers::result_value(result_symbol, parsed))
}
