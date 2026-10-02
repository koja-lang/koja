//! Public error type produced by the LLVM backend.

use std::fmt;
use std::panic::Location;

use inkwell::builder::BuilderError;

/// What can go wrong during [`crate::compile_program`]. Each variant
/// is a short message. The backend does not try to match the
/// `koja_ast::Diagnostic` shape because lowering errors here originate
/// from inkwell or the system target machine, not from user source
/// positions. If a caller needs richer context they can wrap the error
/// themselves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LlvmError {
    /// LLVM refused an operation, or the backend has a gap for a valid
    /// program. Sealed-IR and lowering contract violations panic
    /// instead of surfacing here.
    Codegen(String),
    /// Target machine setup failed or `write_to_file` rejected the
    /// produced module.
    ObjectEmit(String),
}

impl fmt::Display for LlvmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LlvmError::Codegen(msg) => write!(f, "LLVM codegen failed: {msg}"),
            LlvmError::ObjectEmit(msg) => write!(f, "LLVM object emit failed: {msg}"),
        }
    }
}

impl std::error::Error for LlvmError {}

/// Lift an inkwell [`BuilderError`] into [`LlvmError::Codegen`]. A
/// builder failure is always an internal compiler error (mispositioned
/// builder, type mismatch the emitter constructed), so the useful context is
/// *where*. `#[track_caller]` stamps the message with the emission
/// site's `file:line` instead of hand-written prose that goes stale.
pub(crate) trait IceExt<T> {
    fn or_ice(self) -> Result<T, LlvmError>;
}

impl<T> IceExt<T> for Result<T, BuilderError> {
    #[track_caller]
    fn or_ice(self) -> Result<T, LlvmError> {
        let at = Location::caller();
        self.map_err(|e| LlvmError::Codegen(format!("inkwell rejected build at {at}: {e}")))
    }
}
