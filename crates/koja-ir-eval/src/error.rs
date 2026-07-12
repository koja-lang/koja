//! Runtime errors raised by the interpreter. Covers panics (user
//! `Kernel.panic` calls and arithmetic faults) and type mismatches.
//! Anything that would indicate a malformed `IRProgram` (undefined
//! `ValueId`, missing entry, etc.) is a seal violation upstream and
//! panics through `koja_ir::seal`, never surfaces here.

use std::fmt;

use koja_ir::ValueId;

#[derive(Debug, Clone, PartialEq)]
pub enum RuntimeError {
    /// A binary or unary operator received operands whose runtime
    /// types it cannot combine.
    TypeMismatch { detail: String },
    /// `Kernel.panic(message)` was called, or an arithmetic fault
    /// (overflow, zero divisor, non-finite float result) trapped.
    /// The message is preserved verbatim so tests / callers can
    /// assert on it (mirrors the LLVM backend's `__koja_panic`
    /// runtime helper, which prints `** (panic) <message>` plus a
    /// backtrace before aborting).
    Panicked { message: String },
    /// An `@intrinsic`-tagged call resolved to a mangled symbol the
    /// interpreter has no registered handler for. Indicates a missing
    /// registration in `crate::intrinsics`, not a user error.
    UnknownIntrinsic { symbol: String },
    /// An `@extern "C"` (FFI-linked) function was called whose C
    /// symbol isn't registered in [`crate::externs::dispatch`]. The
    /// eval backend exposes a curated subset of `koja-runtime`
    /// symbols (the ones the auto-imported stdlib needs). Calls
    /// outside that subset surface this error so users see exactly
    /// which symbol needs a handler instead of a silent `Unit`
    /// return.
    ExternNotSupported { symbol: String },
    /// Reached an `IRTerminator::Unreachable`. Lowering only emits
    /// these on the failure edge of an exhaustive `match`, so
    /// hitting one means typecheck's exhaustiveness analysis is
    /// wrong (or the IR was constructed by hand outside the
    /// pipeline).
    UnreachableExecuted,
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
            RuntimeError::TypeMismatch { detail } => write!(f, "type mismatch: {detail}"),
            RuntimeError::Panicked { message } => write!(f, "** (panic) {message}"),
            RuntimeError::UnknownIntrinsic { symbol } => {
                write!(
                    f,
                    "unknown intrinsic `{symbol}`: no eval handler registered"
                )
            }
            RuntimeError::ExternNotSupported { symbol } => {
                write!(
                    f,
                    "extern \"C\" `{symbol}` is not registered in the eval \
                     dispatch table. Use --backend=llvm or add a handler \
                     in `koja-ir-eval/src/externs`",
                )
            }
            RuntimeError::UnreachableExecuted => write!(
                f,
                "control reached `IRTerminator::Unreachable`: an exhaustive \
                 match's failure edge fired at runtime",
            ),
            RuntimeError::Unsupported { detail } => write!(f, "unsupported: {detail}"),
            RuntimeError::ValueUndefined { id } => {
                write!(f, "undefined SSA value `{id}`")
            }
        }
    }
}

impl std::error::Error for RuntimeError {}
