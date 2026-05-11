//! Resolver state threaded through every name-resolution recursion.

use expo_ast::identifier::{GlobalRegistryId, ResolvedType};

use crate::pipeline::local_scope::LocalScope;
use crate::registry::GlobalRegistry;

/// File-level resolver inputs: the cross-function pieces every
/// per-function [`Resolver`] reuses verbatim. Bundling them at the
/// file level keeps the `walker::resolve_function` signature short
/// (rather than threading `package` / `registry` positionally
/// through every per-function call), and gives the per-function
/// helper one place to mint a [`Resolver`] from the shared inputs
/// plus its local-scope.
pub(super) struct ResolverEnv<'a> {
    pub package: &'a str,
    pub registry: &'a GlobalRegistry,
}

impl<'a> ResolverEnv<'a> {
    /// Mint a per-function [`Resolver`] reborrowing this env's
    /// shared state alongside the function's `enclosing_type` and
    /// freshly-constructed local scope.
    pub(super) fn make_resolver<'b>(
        &'b mut self,
        enclosing_type: Option<&'b str>,
        type_param_owners: &'b [GlobalRegistryId],
        scope: &'b mut LocalScope,
    ) -> Resolver<'b> {
        Resolver {
            current_return_type: None,
            enclosing_type,
            package: self.package,
            registry: self.registry,
            scope,
            type_param_owners,
        }
    }
}

/// State a name lookup consults: the in-scope package, the global
/// registry, the per-function [`LocalScope`], and the enclosing
/// type's name when the function being resolved is a method.
///
/// `scope` is the per-function locals map (every walker may bind
/// new ids and look existing ones up). User-visible sinks (like
/// diagnostics) stay positional so call signatures advertise that
/// they emit; literal-fit coercions stamp directly onto the AST
/// node (`Expr.literal_coercion`) so no resolver-level sink is
/// needed.
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
    /// Return type of the innermost enclosing function-shape — the
    /// outer `fn` initially, swapped to a closure's return when its
    /// body resolves and restored on the way out. Threaded into
    /// every `Statement::Return`'s value as the bidirectional hint
    /// so things like `return Option.None` pick up the surrounding
    /// `Option<T>` instead of bottoming out at `Option<?>`. Owned
    /// (rather than `&'a ResolvedType`) so closure save/restore can
    /// `mem::replace` without a borrowed-vs-owned mismatch.
    pub current_return_type: Option<ResolvedType>,
    pub enclosing_type: Option<&'a str>,
    pub package: &'a str,
    pub registry: &'a GlobalRegistry,
    pub scope: &'a mut LocalScope,
    /// Owner chain that any in-body type annotation resolves against
    /// — innermost first (function's own id when it declares
    /// type-params, then receiver). Mirrors
    /// `lift_signatures::functions::type_param_owners`; populated
    /// once per [`make_resolver`] call so statement-level helpers
    /// can pass it straight to [`crate::pipeline::lift_signatures::TypeParamScope::new`]
    /// without rebuilding the chain.
    pub type_param_owners: &'a [GlobalRegistryId],
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
