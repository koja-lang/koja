//! Synthesize sub-pass: generate AST for default protocol implementations
//! (Debug, Equality, etc.) on every surviving type that doesn't already
//! provide one.
//!
//! Runs after [`crate::collect`] (so all surviving types are registered)
//! and before [`crate::lift_signatures`] (so the synthesized fns get
//! their signatures lifted in the same way as user-written ones).
//!
//! Today this is a no-op — protocol-default synthesis lands in a later
//! slice. The pass exists in the pipeline so when synthesis arrives
//! the orchestration in [`crate::check_program`] does not change shape.

use expo_ast::ast::Diagnostic;

use crate::program::CheckedPackage;
use crate::registry::GlobalRegistry;

pub(crate) fn synthesize(
    packages: Vec<CheckedPackage>,
    _registry: &mut GlobalRegistry,
    _diagnostics: &mut Vec<Diagnostic>,
) -> Vec<CheckedPackage> {
    packages
}
