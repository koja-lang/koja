//! String-interpolation helpers for [`crate::Interp`] -- the runtime
//! side of [`expo_ir::IRInstruction::StringFormat`].
//!
//! Walks the lowered template parts (literal text + already-lowered
//! holes) and assembles the concatenated `Value::String`. The
//! optional source-level `format` hint (`#{x:%.2f}`) is carried in
//! the IR shape but ignored here -- the interpreter renders every
//! hole through [`format_interp_value`], matching what the
//! codegen `compile_string` does for non-printf-spec holes
//! (round-trip through `call_format`).

use std::fmt::Write;
use std::rc::Rc;

use expo_ir::values::StringFormatPart;

use crate::error::RuntimeError;
use crate::frame::Frame;
use crate::value::Value;

/// Build a [`Value::String`] from the lowered template parts. Each
/// [`StringFormatPart::Literal`] is appended verbatim; each
/// [`StringFormatPart::Interpolated`] hole materializes its operand
/// against `frame` and renders via [`format_interp_value`].
pub(crate) fn format_string(
    frame: &Frame,
    parts: &[StringFormatPart],
) -> Result<Value, RuntimeError> {
    let mut buf = String::new();
    for part in parts {
        match part {
            StringFormatPart::Literal(text) => buf.push_str(text),
            StringFormatPart::Interpolated { value, .. } => {
                let resolved = frame.materialize(value)?;
                format_interp_value(&mut buf, &resolved)?;
            }
        }
    }
    Ok(Value::String(Rc::new(buf)))
}

/// Render a [`Value`] as it should appear inside an interpolated
/// string. Mostly delegates to [`Value::Display`], but special-cases
/// [`Value::String`]: the `Display` impl wraps strings in quotes
/// (Rust `{:?}` style), which is the wrong shape for source-level
/// interpolation -- `"hello, #{name}"` with `name = "Alice"` should
/// render as `hello, Alice`, not `hello, "Alice"`.
fn format_interp_value(buf: &mut String, value: &Value) -> Result<(), RuntimeError> {
    match value {
        Value::String(s) => buf.push_str(s),
        other => {
            write!(buf, "{other}").map_err(|e| {
                RuntimeError::Unsupported(format!("string interpolation: write failed ({e})"))
            })?;
        }
    }
    Ok(())
}
