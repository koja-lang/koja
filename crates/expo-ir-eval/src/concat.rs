//! Concatenation helpers for [`crate::Interp`] -- the runtime side of
//! [`expo_ir::IRInstruction::Concat`].
//!
//! Pure functions over already-materialized [`Value`] parts; the
//! interpreter's per-instruction dispatch resolves the operands first
//! (via [`crate::frame::Frame::materialize`]) and hands the resulting
//! `Vec<Value>` here.
//!
//! The two strategies map 1:1 to the codegen
//! `compile_string_concat` / `compile_binary_concat` helpers --
//! [`expo_ir::resolved::strings::ResolvedConcatKind`] is the discriminator
//! both backends share, decided at lowering time so neither has to
//! re-derive it from runtime value shapes.

use std::rc::Rc;

use crate::error::RuntimeError;
use crate::value::Value;

/// Fold N already-materialized [`Value::String`] parts into a single
/// `Value::String`. Mirrors the codegen `compile_string_concat`'s
/// payload assembly (sans the bit-length header / NUL terminator,
/// which are LLVM ABI concerns the interpreter doesn't need).
pub(crate) fn concat_strings(parts: Vec<Value>) -> Result<Value, RuntimeError> {
    let mut buf = String::new();
    for part in parts {
        let Value::String(s) = part else {
            return Err(RuntimeError::TypeMismatch(format!(
                "string concat expected Value::String operand, got {part:?}"
            )));
        };
        buf.push_str(&s);
    }
    Ok(Value::String(Rc::new(buf)))
}

/// Fold N already-materialized [`Value::Binary`] parts into a single
/// `Value::Binary`. Mirrors `compile_binary_concat`.
pub(crate) fn concat_binaries(parts: Vec<Value>) -> Result<Value, RuntimeError> {
    let total: usize = parts
        .iter()
        .map(|v| match v {
            Value::Binary(bytes) => Ok(bytes.len()),
            other => Err(RuntimeError::TypeMismatch(format!(
                "binary concat expected Value::Binary operand, got {other:?}"
            ))),
        })
        .sum::<Result<usize, RuntimeError>>()?;
    let mut buf = Vec::with_capacity(total);
    for part in parts {
        let Value::Binary(bytes) = part else {
            unreachable!("type-checked above");
        };
        buf.extend_from_slice(&bytes);
    }
    Ok(Value::Binary(Rc::new(buf)))
}
