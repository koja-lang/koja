//! Cross-phase enforcement of `priv` on non-call reference sites.
//!
//! Call sites enforce `priv fn` in `resolve::calls` (see
//! `check_callee_visibility` there). This module is the equivalent
//! seam for every other reference position: type expressions in
//! signatures (`lift_signatures`), constructors / patterns / static
//! receivers (`resolve`), `extend` targets (`collect`), and `alias`
//! targets (`aliases`).

use koja_ast::ast::Diagnostic;
use koja_ast::span::Span;

use crate::registry::{RegistryEntry, VisibilityScope};

/// Enforce a decl's [`VisibilityScope`] at a reference site. A
/// violation pushes one diagnostic and resolution proceeds, so
/// callers see exactly one error per offending site and downstream
/// passes walk a populated tree. Only `PackagePrivate` can fire
/// here. `TypePrivate` exists solely for functions, which are
/// gated at call sites.
pub(crate) fn check_reference_visibility(
    entry: &RegistryEntry,
    referrer_package: &str,
    span: Span,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if entry.visibility != VisibilityScope::PackagePrivate
        || entry.identifier.package() == referrer_package
    {
        return;
    }
    diagnostics.push(Diagnostic::error_with_hint(
        format!(
            "private {} `{}` cannot be referenced from package `{referrer_package}`",
            entry.kind.label(),
            entry.identifier,
        ),
        format!(
            "`{}` is `priv`, usable only from package `{}` (declared at line {})",
            entry.identifier,
            entry.identifier.package(),
            entry.span.start.line,
        ),
        span,
    ));
}
