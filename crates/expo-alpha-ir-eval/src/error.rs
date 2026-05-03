//! Runtime errors raised by the interpreter.
//!
//! These are conditions the interpreter can recover diagnostics from
//! at runtime — division by zero, integer overflow, type mismatches.
//! Anything that would indicate a malformed `IRProgram` (undefined
//! `ValueId`, missing entry point, etc.) is a seal violation upstream
//! and panics through `expo_alpha_ir::seal`, never surfaces here.

use expo_alpha_ir::{IRBinOp, ValueId};

#[derive(Debug, Clone, PartialEq)]
pub enum RuntimeError {
    /// `lhs / rhs` or `lhs % rhs` with `rhs == 0`.
    DivisionByZero { op: IRBinOp },
    /// Integer arithmetic produced a value outside the `i64` range.
    IntegerOverflow { lhs: i64, op: IRBinOp, rhs: i64 },
    /// A binary operator received operands whose runtime types it
    /// cannot combine. The POC eval only knows `Int op Int`.
    TypeMismatch { detail: String },
    /// Catch-all for IR shapes the interpreter doesn't yet handle.
    Unsupported { detail: String },
    /// An operand referenced a `ValueId` not yet defined in the
    /// current frame. Should never happen on a sealed program; if it
    /// does, the IRProgram seal contract was violated.
    ValueUndefined { id: ValueId },
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RuntimeError::DivisionByZero { op } => {
                write!(f, "{op:?} by zero")
            }
            RuntimeError::IntegerOverflow { lhs, op, rhs } => {
                write!(f, "integer overflow: {lhs} {op:?} {rhs}")
            }
            RuntimeError::TypeMismatch { detail } => write!(f, "type mismatch: {detail}"),
            RuntimeError::Unsupported { detail } => write!(f, "unsupported: {detail}"),
            RuntimeError::ValueUndefined { id } => {
                write!(f, "undefined SSA value `{id}`")
            }
        }
    }
}

impl std::error::Error for RuntimeError {}
