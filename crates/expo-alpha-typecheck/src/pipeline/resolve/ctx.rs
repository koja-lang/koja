//! Resolver state threaded through every name-resolution recursion.

use expo_ast::identifier::GlobalRegistryId;

use crate::pipeline::local_scope::LocalScope;
use crate::registry::GlobalRegistry;

/// State a name lookup consults: the in-scope package, the global
/// registry, and the per-function [`LocalScope`]. Diagnostics is
/// intentionally **not** here — sinks live positionally so callers
/// see "this can emit errors" in their signatures.
///
/// Pure data bundle by convention: no `impl` block. Helpers reach
/// for fields directly (`resolver.registry`, `resolver.scope`) so
/// each callee is honest about what it actually uses.
pub(super) struct Resolver<'a> {
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
