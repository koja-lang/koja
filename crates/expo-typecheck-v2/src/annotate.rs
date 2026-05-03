//! Annotate sub-pass: emit any remaining annotations on the resolved
//! AST that downstream passes need but that fall outside `Resolution`
//! and `Expr.resolved_type` (today that means coercion annotations on
//! sites where an implicit conversion is required).
//!
//! Runs after [`crate::check`] so type compatibility is already
//! validated; runs before [`crate::seal`] so seal can include
//! annotation-presence checks once the contract is defined.
//!
//! Today this is a no-op — coercion annotation lands in a later slice.

use expo_ast::ast::Diagnostic;

use crate::program::CheckedPackage;
use crate::registry::GlobalRegistry;

pub(crate) fn annotate(
    packages: Vec<CheckedPackage>,
    _registry: &GlobalRegistry,
    _diagnostics: &mut Vec<Diagnostic>,
) -> Vec<CheckedPackage> {
    packages
}
