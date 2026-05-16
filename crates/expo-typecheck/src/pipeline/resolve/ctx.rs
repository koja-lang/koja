//! Resolver state threaded through every name-resolution recursion.

use expo_ast::ast::{AliasDecl, PassMode};
use expo_ast::identifier::{GlobalRegistryId, ResolvedType};

use crate::pipeline::lift_signatures::ResolutionScope;
use crate::pipeline::local_scope::LocalScope;
use crate::registry::GlobalRegistry;

use super::moves::MoveLedger;

/// File-level resolver inputs: the cross-function pieces every
/// per-function [`Resolver`] reuses verbatim. Bundling them at the
/// file level keeps the `walker::resolve_function` signature short
/// (rather than threading `package` / `registry` positionally
/// through every per-function call), and gives the per-function
/// helper one place to mint a [`Resolver`] from the shared inputs
/// plus its local-scope.
///
/// `file_aliases` is the per-file alias roster powering
/// `alias`-prefixed type name resolution inside function bodies.
pub(super) struct ResolverEnv<'a> {
    pub file_aliases: &'a [AliasDecl],
    pub package: &'a str,
    pub registry: &'a GlobalRegistry,
}

impl<'a> ResolverEnv<'a> {
    /// Mint a per-function [`Resolver`] reborrowing this env's
    /// shared state alongside the function's `enclosing_type` and
    /// freshly-constructed local scope. `enclosing_type_id` is the
    /// receiver type's [`GlobalRegistryId`] when the function is a
    /// method on a known type (parallel to `enclosing_type`'s
    /// name); `None` for top-level fns and file bodies. It anchors
    /// the `priv fn` type-private visibility check.
    pub(super) fn make_resolver<'b>(
        &'b mut self,
        enclosing_type: Option<&'b str>,
        enclosing_type_id: Option<GlobalRegistryId>,
        self_pass_mode: Option<PassMode>,
        type_param_owners: &'b [GlobalRegistryId],
        scope: &'b mut LocalScope,
    ) -> Resolver<'b> {
        Resolver {
            current_return_type: None,
            enclosing_type,
            enclosing_type_id,
            file_aliases: self.file_aliases,
            loop_break_seen: Vec::new(),
            loop_depth: 0,
            moves: MoveLedger::new(),
            package: self.package,
            registry: self.registry,
            scope,
            self_pass_mode,
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
/// each callee is honest about what it actually uses. The one
/// exception is [`Self::resolution_scope`] — bundling the three
/// type-resolution inputs in one place keeps every
/// `resolve_type_expr` / `lookup_type` call short and rules out
/// "passed `package` from one resolver and `aliases` from
/// another" mismatches at the call site.
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
    /// Registry id of the enclosing type when the resolver is
    /// walking a method body, parallel to `enclosing_type`'s name.
    /// `None` for top-level functions and the file body. Anchors
    /// the `priv fn` type-private check — a `TypePrivate(owner)`
    /// callee is only callable when this equals `Some(owner)`.
    pub enclosing_type_id: Option<GlobalRegistryId>,
    /// In-scope alias roster for the current file, validated by
    /// [`crate::pipeline::aliases::validate_aliases`]. Consulted by
    /// every type-name lookup before falling back to the current
    /// package and `Global` (the lookup precedence).
    pub file_aliases: &'a [AliasDecl],
    /// One slot per enclosing `loop` / `while`, set to `true` by
    /// the `break` gate when a break targets the innermost loop.
    /// `resolve_loop` consults the popped slot to decide whether
    /// the loop expression types as `Unit` (saw break) or `Never`
    /// (no break — divergent). Closure boundaries replace this
    /// stack with an empty one and restore on exit so an inner
    /// `break` can never mark an outer-function loop.
    pub loop_break_seen: Vec<bool>,
    /// Nesting depth of enclosing `loop` / `while` bodies. `break`
    /// is only legal when this is `> 0`. Closure boundaries reset
    /// this to `0` and restore on exit so a `break` inside a
    /// closure body must reference a loop _inside_ the closure.
    pub loop_depth: u32,
    /// Per-function move-state ledger. Updated at every move-trigger
    /// site (assignment RHS for non-`Copy` types, `move` parameter
    /// arg, `move self` receiver) and consulted at every local read
    /// to diagnose use-after-move. Lives on the resolver so branch
    /// joins can `snapshot` / `restore` / `merge_branches` it
    /// without separate plumbing.
    pub moves: MoveLedger,
    pub package: &'a str,
    pub registry: &'a GlobalRegistry,
    pub scope: &'a mut LocalScope,
    /// `PassMode` of the enclosing method's `self` receiver, when one
    /// exists. `Some(PassMode::Move)` admits `self.field = …` mutation;
    /// `Some(PassMode::Borrow | PassMode::Copy)` rejects it. `None`
    /// outside any method (top-level fns, file body) — there's no
    /// `self` in scope at all, so the field-assignment self-mutation
    /// rule trivially doesn't apply (the head-local lookup already
    /// fails, and the assignment diagnoses as "undeclared").
    pub self_pass_mode: Option<PassMode>,
    /// Owner chain that any in-body type annotation resolves against
    /// — innermost first (function's own id when it declares
    /// type-params, then receiver). Mirrors
    /// `lift_signatures::functions::type_param_owners`; populated
    /// once per [`make_resolver`] call so statement-level helpers
    /// can pass it straight to [`crate::pipeline::lift_signatures::TypeParamScope::new`]
    /// without rebuilding the chain.
    pub type_param_owners: &'a [GlobalRegistryId],
}

impl<'a> Resolver<'a> {
    /// Project the type-resolution inputs (alias slice + current
    /// package + registry) into a [`ResolutionScope`] for handing
    /// off to `resolve_type_expr` / `lookup_type`. Returns a scope
    /// tied to the resolver's own lifetime `'a` (not `&self`'s
    /// lifetime), so callers can hold the scope across subsequent
    /// `&mut resolver` calls without tripping the borrow checker —
    /// the inner field reborrows are always valid for `'a`.
    pub(super) fn resolution_scope(&self) -> ResolutionScope<'a> {
        ResolutionScope {
            aliases: self.file_aliases,
            package: self.package,
            registry: self.registry,
        }
    }
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
