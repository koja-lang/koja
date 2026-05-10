//! Resolver state threaded through every name-resolution recursion.

use expo_ast::identifier::GlobalRegistryId;

use crate::pipeline::local_scope::LocalScope;
use crate::registry::GlobalRegistry;

/// State a name lookup consults: the in-scope package, the global
/// registry, the per-function [`LocalScope`], and the enclosing
/// type's name when the function being resolved is a method.
/// Diagnostics is intentionally **not** here — sinks live
/// positionally so callers see "this can emit errors" in their
/// signatures.
///
/// `enclosing_type` is the unqualified name of the function's
/// owner — `"DateTime"` for a method on `Global.DateTime`, `None`
/// for top-level free functions and file bodies. It encodes the
/// language's bare-call lookup rule: **prioritize your enclosing
/// scope, then fall back to package scope**. So inside
/// `System.cwd`, bare `expo_cwd()` resolves to the sibling
/// `Global.System.expo_cwd` first; only if no sibling matches
/// does the resolver consult `Global.expo_cwd`. Conflicts are
/// resolved in favor of the enclosing scope; the escape hatch
/// for callers who really want the package-level function is to
/// fully qualify (`Global.expo_cwd()`), which goes through path-
/// call resolution and bypasses bare lookup entirely. The same
/// rule generalizes when nested types land: each level wins over
/// the next outward one.
///
/// Pure data bundle by convention: no `impl` block. Helpers reach
/// for fields directly (`resolver.registry`, `resolver.scope`) so
/// each callee is honest about what it actually uses.
pub(super) struct Resolver<'a> {
    pub enclosing_type: Option<&'a str>,
    pub package: &'a str,
    pub registry: &'a GlobalRegistry,
    pub scope: &'a mut LocalScope,
}

/// Registry-side metadata for one inference target — bundled so
/// the call / struct / enum inference helpers stay under the
/// `too_many_arguments` threshold without inventing a
/// `(registry, diagnostics)` pair. Holds borrowed views of the
/// [`crate::registry::RegistryEntry`]'s identifier (`label`) and
/// `type_params`, plus its [`GlobalRegistryId`] (the substitution
/// owner). Method-call inference uses two side by side — one for
/// the method, one for the enclosing type.
#[derive(Clone, Copy)]
pub(super) struct Callee<'a> {
    pub id: GlobalRegistryId,
    pub label: &'a str,
    pub type_params: &'a [String],
}
