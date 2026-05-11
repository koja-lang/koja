//! `Int.parse(input: String) -> Result<Int, String>` and
//! `Float.parse(input: String) -> Result<Float, String>`.
//!
//! Eval mirrors what the LLVM backend would emit if the runtime
//! parse helpers landed: trim leading / trailing whitespace, hand
//! the rest to Rust's `str::parse::<i64>()` / `::<f64>()`, and
//! materialize the resulting `Result` directly. The Result enum's
//! symbol comes from `function.return_type`; `Ok` / `Err` variants
//! follow v1's tag convention (`Ok = 0`, `Err = 1`) the same way
//! `Option` does in `list.rs` / `string.rs`.

use expo_alpha_ir::{IRFunction, ParseTarget};

use crate::error::RuntimeError;
use crate::intrinsics::helpers;
use crate::value::Value;

pub(super) fn dispatch(
    target: ParseTarget,
    function: &IRFunction,
    args: &[Value],
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
    let parsed = match std::str::from_utf8(bytes) {
        Ok(s) => {
            let trimmed = s.trim();
            match target {
                ParseTarget::Int => parse_int(trimmed),
                ParseTarget::Float => parse_float(trimmed),
            }
        }
        Err(err) => Err(Value::String(
            format!(
                "{target:?}.parse: input is not valid UTF-8 (invalid at byte {}): {err}",
                err.valid_up_to(),
            )
            .into_bytes(),
        )),
    };
    Ok(helpers::result_value(result_symbol, parsed))
}

fn parse_int(text: &str) -> Result<Value, Value> {
    match text.parse::<i64>() {
        Ok(v) => Ok(Value::Int(v)),
        Err(err) => Err(Value::String(
            format!("Int.parse: `{text}` is not a valid Int ({err})").into_bytes(),
        )),
    }
}

fn parse_float(text: &str) -> Result<Value, Value> {
    match text.parse::<f64>() {
        Ok(v) => Ok(Value::Float64(v)),
        Err(err) => Err(Value::String(
            format!("Float.parse: `{text}` is not a valid Float ({err})").into_bytes(),
        )),
    }
}
