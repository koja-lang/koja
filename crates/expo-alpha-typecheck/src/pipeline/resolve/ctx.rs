//! Resolver state threaded through every name-resolution recursion.

use expo_ast::identifier::GlobalRegistryId;

use crate::pipeline::local_scope::LocalScope;
use crate::registry::GlobalRegistry;

use super::coercion::Coercions;

/// File-level resolver inputs: the cross-function pieces every
/// per-function [`Resolver`] reuses verbatim. Bundling them at the
/// file level keeps the `walker::resolve_function` signature short
/// (rather than threading `package` / `registry` / `coercions`
/// positionally through every per-function call), and gives the
/// per-function helper one place to mint a [`Resolver`] from the
/// shared inputs plus its local-scope.
///
/// `coercions` is the program-wide span-keyed numeric-literal
/// coercion table — populated by the type-equality leaves
/// ([`super::structs::validate_named_fields`],
/// [`super::calls::validate_arg_signature`] et al.) when a literal
/// flows into a narrower-than-default sized target, consumed by
/// `expo-alpha-ir`'s expression lowerer to mint the `Const`
/// instruction at the recorded width.
pub(super) struct ResolverEnv<'a> {
    pub coercions: &'a mut Coercions,
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
        scope: &'b mut LocalScope,
    ) -> Resolver<'b> {
        Resolver {
            coercions: &mut *self.coercions,
            enclosing_type,
            package: self.package,
            registry: self.registry,
            scope,
        }
    }
}

/// State a name lookup consults: the in-scope package, the global
/// registry, the per-function [`LocalScope`], the enclosing
/// type's name when the function being resolved is a method, and
/// the program-wide [`Coercions`] sink shared with siblings.
///
/// Two mutable handles ride alongside the read-only state.
/// `scope` is the per-function locals map (every walker may bind
/// new ids and look existing ones up). `coercions` reaches every
/// type-equality leaf so a literal-fit coercion can be recorded
/// without fanning a `&mut Coercions` argument through every
/// `resolve_expr` recursion; user-visible sinks (like diagnostics)
/// stay positional so call signatures advertise that they emit.
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
    pub coercions: &'a mut Coercions,
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
