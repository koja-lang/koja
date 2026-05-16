//! User-actionable failure mode for the typecheck phase.
//!
//! [`crate::check_program`] returns `Result<CheckedProgram,
//! CheckFailure>`. The failure variant carries:
//!
//! - `diagnostics` — typecheck-emitted diagnostics. Parse-emitted
//!   diagnostics live on `partial.iter().flat_map(|f| &f.diagnostics)`;
//!   when the parser already produced error-severity diagnostics,
//!   typecheck halts early and `diagnostics` is empty.
//! - `partial` — best-effort reconstructed [`ParsedProgram`]. **Not
//!   sealed** — its annotations are whatever resolve managed to
//!   stamp before halting. LSPs and `expo check` consume this
//!   for partial diagnostics rendering.
//!
//! Compiler-bug failure modes (seal invariant violations) panic
//! through [`crate::pipeline::seal`] and never surface here.

use expo_ast::ast::Diagnostic;
use expo_parser::ParsedProgram;

/// Failure result of [`crate::check_program`].
///
/// `diagnostics` carries only the diagnostics typecheck emitted.
/// Parse diagnostics live on `partial.iter().flat_map(|f|
/// &f.diagnostics)`. When the parser already produced error-severity
/// diagnostics, typecheck halts early and `diagnostics` is empty.
/// The partial AST is **not** sealed.
#[derive(Debug)]
pub struct CheckFailure {
    pub diagnostics: Vec<Diagnostic>,
    pub partial: ParsedProgram,
}

impl std::fmt::Display for CheckFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for diag in &self.diagnostics {
            writeln!(f, "{}", diag.message)?;
        }
        Ok(())
    }
}

impl std::error::Error for CheckFailure {}
