//! Resolver state threaded through every name-resolution recursion.

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
