//! Public error type produced by the alpha LLVM backend.

use std::fmt;

/// What can go wrong during [`crate::compile_program`]. Each variant
/// is a short message; we don't try to match `expo_ast::Diagnostic`
/// shape because lowering errors here originate from inkwell or the
/// system target machine, not from user source positions. If a
/// caller needs richer context they can wrap the error themselves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LlvmError {
    /// Failed to emit LLVM IR for an IRProgram — e.g. a feature-gap
    /// instruction or terminator was encountered.
    Codegen(String),
    /// Target machine setup failed or `write_to_file` rejected the
    /// produced module.
    ObjectEmit(String),
}

impl fmt::Display for LlvmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LlvmError::Codegen(msg) => write!(f, "alpha LLVM codegen failed: {msg}"),
            LlvmError::ObjectEmit(msg) => write!(f, "alpha LLVM object emit failed: {msg}"),
        }
    }
}

impl std::error::Error for LlvmError {}
