//! Runtime errors raised by the interpreter — recoverable diagnostics
//! like division by zero, integer overflow, and type mismatches.
//! Anything that would indicate a malformed `IRProgram` (undefined
//! `ValueId`, missing entry, etc.) is a seal violation upstream and
//! panics through `expo_alpha_ir::seal`, never surfaces here.

use std::fmt;

use expo_alpha_ir::{IRBinOp, IRUnaryOp, ValueId};

#[derive(Debug, Clone, PartialEq)]
pub enum RuntimeError {
    /// `lhs / rhs` or `lhs % rhs` with `rhs == 0`.
    DivisionByZero { op: IRBinOp },
    /// Integer arithmetic produced a value outside the `i64` range.
    IntegerOverflow { lhs: i64, op: IRBinOp, rhs: i64 },
    /// A binary or unary operator received operands whose runtime
    /// types it cannot combine.
    TypeMismatch { detail: String },
    /// A unary operator produced a value outside the `i64` range
    /// (in practice: negating `i64::MIN`).
    UnaryIntegerOverflow { op: IRUnaryOp, operand: i64 },
    /// Catch-all for IR shapes the interpreter doesn't yet handle.
    Unsupported { detail: String },
    /// An operand referenced a `ValueId` not yet defined in the
    /// current frame. Seal contract violation if it happens on a
    /// sealed program.
    ValueUndefined { id: ValueId },
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RuntimeError::DivisionByZero { op } => {
                write!(f, "{op:?} by zero")
            }
            RuntimeError::IntegerOverflow { lhs, op, rhs } => {
                write!(f, "integer overflow: {lhs} {op:?} {rhs}")
            }
            RuntimeError::TypeMismatch { detail } => write!(f, "type mismatch: {detail}"),
            RuntimeError::UnaryIntegerOverflow { op, operand } => {
                write!(f, "integer overflow: {op:?} {operand}")
            }
            RuntimeError::Unsupported { detail } => write!(f, "unsupported: {detail}"),
            RuntimeError::ValueUndefined { id } => {
                write!(f, "undefined SSA value `{id}`")
            }
        }
    }
}

impl std::error::Error for RuntimeError {}
