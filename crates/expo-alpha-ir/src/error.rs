//! User-actionable failure modes for the alpha lowering phase.
//!
//! Both lowering entry points ([`crate::lower_program`] and
//! [`crate::lower_script`]) return `Result<_, LowerError>`. The
//! variants are disjoint and signal where in the pipeline the
//! failure surfaced — feature-gap diagnostics get accumulated while
//! walking the sealed AST, and the entry-point lookup miss is a
//! project-mode-only concern.
//!
//! Compiler-bug failure modes (seal invariant violations) panic
//! through [`crate::seal`] and never surface here.

use expo_ast::ast::Diagnostic;
use expo_ast::identifier::Identifier;

/// User-actionable failure modes from [`crate::lower_program`] and
/// [`crate::lower_script`]. Anything that could only originate from a
/// compiler bug panics through `seal` instead of surfacing here.
///
/// `Diagnostics` and `EntryPointNotFound` are disjoint: the lowering
/// pass short-circuits before the entry-point check when diagnostics
/// are present, so callers can match on one variant at a time.
/// `EntryPointNotFound` is project-mode-only; script-mode lowering
/// has no caller-named entry to miss.
#[derive(Debug, Clone)]
pub enum LowerError {
    /// One or more feature-gap diagnostics surfaced while lowering
    /// the sealed AST (unsupported expression / literal / statement
    /// kinds, extern-body functions, unsupported binary operators,
    /// etc.). Each [`Diagnostic`] carries a source span + message.
    /// Lowering is per-function fail-fast: a failed function
    /// contributes one diagnostic and is omitted from the resulting
    /// partial IR.
    Diagnostics(Vec<Diagnostic>),
    /// The caller asked for an entry point that no package in the
    /// lowered program registers.
    EntryPointNotFound { identifier: Identifier },
}

impl std::fmt::Display for LowerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LowerError::Diagnostics(diagnostics) => {
                for (index, diag) in diagnostics.iter().enumerate() {
                    if index > 0 {
                        writeln!(f)?;
                    }
                    write!(f, "{}", diag.message)?;
                }
                Ok(())
            }
            LowerError::EntryPointNotFound { identifier } => {
                write!(f, "entry point `{identifier}` is not defined")
            }
        }
    }
}

impl std::error::Error for LowerError {}
