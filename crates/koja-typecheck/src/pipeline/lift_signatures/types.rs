//! Type-expression resolution + small label/span helpers shared by
//! every other submodule under `lift_signatures/`.

use koja_ast::ast::{AliasDecl, Diagnostic, TypeExpr};
use koja_ast::identifier::{
    AnonymousKind, GlobalRegistryId, Identifier, Resolution, ResolvedType, TypeParamIndex,
};
use koja_ast::span::Span;

use crate::pipeline::aliases::rewrite_through_aliases;
use crate::pipeline::resolve::types::canonical_union;
use crate::registry::{Dispatch, GlobalKind, GlobalRegistry};

/// Read-only name-resolution inputs threaded through type-expression
/// resolution. `Copy` so callers pass it by value without ceremony.
///
/// **Do not grow this struct.** It exists to bundle the three
/// pieces `resolve_type_expr` needs to map a `TypeExpr` to a
/// `ResolvedType`: the file's alias slice (file-private), the
/// current package (same-package lookups), and the global registry
/// (everything else). `diagnostics` lives outside on purpose so
/// every emit site is honest; if you find yourself wanting to add
/// a `&mut` field here, you want a different abstraction (likely a
/// separate sink arg, or lift the work out of resolve).
///
/// Sibling of [`TypeParamScope`]: that one models the lexical
/// generic-decl scope (innermost-first stack of owners), this one
/// models the file/package scope (alias roster + current package
/// + global registry). Most resolve helpers take both.
#[derive(Clone, Copy)]
pub(crate) struct ResolutionScope<'a> {
    pub aliases: &'a [AliasDecl],
    pub package: &'a str,
    pub registry: &'a GlobalRegistry,
}

/// Stack of generic-decl owners visible at this resolution site.
/// Innermost first (e.g. `[fn_id, struct_id]` for an inline method
/// on a generic struct). [`Self::lookup`] walks the stack and yields
/// the first `(owner, index)` whose entry registers a matching param
/// name. Names live on the [`GlobalRegistry`] entry — the scope is
/// just the chain.
///
/// Empty scope (`TypeParamScope::default()`) is the right value
/// outside any generic-decl body; lookups against it always return
/// `None` so resolve falls through to global lookup.
#[derive(Clone, Copy, Default)]
pub(crate) struct TypeParamScope<'a> {
    owners: &'a [GlobalRegistryId],
}

impl<'a> TypeParamScope<'a> {
    pub(crate) fn new(owners: &'a [GlobalRegistryId]) -> Self {
        Self { owners }
    }

    pub(crate) fn lookup(
        &self,
        name: &str,
        registry: &GlobalRegistry,
    ) -> Option<(GlobalRegistryId, TypeParamIndex)> {
        for &owner in self.owners {
            let names = registry.type_params(owner)?;
            if let Some(pos) = names.iter().position(|n| n == name) {
                return Some((owner, TypeParamIndex::new(pos as u32)));
            }
        }
        None
    }

    /// Walk innermost-out and return the first owner whose kind owns
    /// a `Self` (protocols, structs, enums). Functions are skipped —
    /// `Self` inside a top-level / inline `fn` looks past it to the
    /// enclosing receiver.
    pub(crate) fn self_owner(&self) -> &'a [GlobalRegistryId] {
        self.owners
    }
}

/// Resolve a [`TypeExpr`] against the registry. Single-segment
/// `TypeExpr::Named` matching the surrounding scope resolves to
/// [`Resolution::TypeParam`]; otherwise [`rewrite_through_aliases`]
/// gets first crack (so an `alias`-bound name resolves to its
/// target package), then we fall back to a preloaded `Global.<name>`
/// stub or a same-package struct/enum. `TypeExpr::Generic` recurses
/// into its args. `type_params` is empty outside generic-decl bodies
/// (see [`TypeParamScope::default`]); `scope` carries the file's
/// alias slice + current package + registry (see
/// [`ResolutionScope`]).
pub(crate) fn resolve_type_expr(
    type_expr: &TypeExpr,
    type_params: TypeParamScope<'_>,
    scope: ResolutionScope<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    match type_expr {
        TypeExpr::Function {
            params,
            return_type,
            ..
        } => {
            let resolved_params = params
                .iter()
                .map(|param_ty| resolve_type_expr(param_ty, type_params, scope, diagnostics))
                .collect();
            let ret = resolve_type_expr(return_type, type_params, scope, diagnostics);
            ResolvedType::Anonymous(AnonymousKind::Function {
                params: resolved_params,
                ret: Box::new(ret),
            })
        }
        TypeExpr::Generic { path, args, span } => {
            resolve_generic(path, args, *span, type_params, scope, diagnostics)
        }
        TypeExpr::Named { path, span } => {
            resolve_named(path, *span, type_params, scope, diagnostics)
        }
        TypeExpr::Self_ { span } => resolve_self(*span, type_params, scope.registry, diagnostics),
        TypeExpr::Union { types, .. } => {
            let members = types
                .iter()
                .map(|t| resolve_type_expr(t, type_params, scope, diagnostics))
                .collect::<Vec<_>>();
            canonical_union(members, scope.registry)
        }
        TypeExpr::Unit { .. } => scope.registry.primitive("Unit"),
    }
}

/// Resolve a bare `Self` type-expression. Walks the scope from
/// innermost outward and dispatches by owner kind: a protocol owner
/// resolves to its implicit slot-0 type-param (protocols register
/// with `["Self", ...declared]`); a struct/enum owner resolves to
/// the type itself with each of its type-params projected as
/// [`Resolution::TypeParam`] anchors so `fn make() -> Self` on
/// `struct Pair<T, U>` reads as `Pair<T, U>`. Functions are skipped
/// — `Self` inside a generic `fn` looks past the fn to its
/// enclosing struct/enum/impl.
fn resolve_self(
    span: Span,
    type_params: TypeParamScope<'_>,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    for &owner in type_params.self_owner() {
        let Some(entry) = registry.get(owner) else {
            continue;
        };
        match &entry.kind {
            GlobalKind::Protocol(_) => {
                return ResolvedType::leaf(Resolution::TypeParam {
                    owner,
                    index: TypeParamIndex::new(0),
                });
            }
            GlobalKind::Struct(_) | GlobalKind::Enum(_) => {
                return concrete_self_type(owner, registry);
            }
            GlobalKind::Constant(_) | GlobalKind::Function(_) | GlobalKind::TypeAlias(_) => {
                continue;
            }
        }
    }
    diagnostics.push(Diagnostic::error(
        "`Self` is only valid inside a protocol, struct, enum, or impl block".to_string(),
        span,
    ));
    ResolvedType::unresolved()
}

/// Build a `ResolvedType` for `Self` in a struct/enum context: the
/// type itself with each of its declared type-params projected as a
/// `TypeParam(owner, i)` so monomorphization substitutes them
/// alongside every other body resolution naming the same param.
/// Shared between `Self` resolution and the `self` receiver lifter
/// in [`super::functions`] so both produce identical receiver types
/// for generic-target methods.
pub(crate) fn concrete_self_type(
    owner: GlobalRegistryId,
    registry: &GlobalRegistry,
) -> ResolvedType {
    let arity = registry
        .type_params(owner)
        .map(<[String]>::len)
        .unwrap_or(0);
    let type_args = (0..arity)
        .map(|i| {
            ResolvedType::leaf(Resolution::TypeParam {
                owner,
                index: TypeParamIndex::new(i as u32),
            })
        })
        .collect();
    ResolvedType::Named {
        resolution: Resolution::Global(owner),
        type_args,
    }
}

/// Resolve `Path<args...>`. Path resolution mirrors [`resolve_named`]
/// for the head — type-param scope wins, then file aliases, then
/// the same-package / `Global` fallthrough. Type args lower
/// recursively through the same scope. A type param shadows a
/// global of the same name; `T<args>` is an error because type
/// params are arity-0. Aliases resolve straight to their target
/// `Identifier`, sidestepping the dotted-path "no nested types"
/// gate so `alias Some.Outer as O` followed by `O<Int>` works as
/// soon as the registry carries the target — no movement here when
/// nested-type lifting lands.
fn resolve_generic(
    path: &[String],
    args: &[TypeExpr],
    span: Span,
    type_params: TypeParamScope<'_>,
    scope: ResolutionScope<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    if path.len() == 1 && type_params.lookup(&path[0], scope.registry).is_some() {
        diagnostics.push(Diagnostic::error(
            format!(
                "typecheck: type parameter `{}` cannot take type arguments",
                path[0],
            ),
            span,
        ));
        return ResolvedType::unresolved();
    }
    let head = match resolve_path_to_global(path, span, scope, diagnostics) {
        Some(id) => Resolution::Global(id),
        None => return ResolvedType::unresolved(),
    };
    let resolved_args = args
        .iter()
        .map(|arg| resolve_type_expr(arg, type_params, scope, diagnostics))
        .collect();
    ResolvedType::Named {
        resolution: head,
        type_args: resolved_args,
    }
}

fn resolve_named(
    path: &[String],
    span: Span,
    type_params: TypeParamScope<'_>,
    scope: ResolutionScope<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    if path.len() == 1
        && let Some((owner, index)) = type_params.lookup(&path[0], scope.registry)
    {
        return ResolvedType::leaf(Resolution::TypeParam { owner, index });
    }
    match resolve_path_to_global(path, span, scope, diagnostics) {
        Some(id) => ResolvedType::leaf(Resolution::Global(id)),
        None => ResolvedType::unresolved(),
    }
}

/// Resolve a (possibly multi-segment) type path to its registry id,
/// emitting a diagnostic and returning `None` on miss. Lookup
/// precedence:
///
/// 1. A file alias on the head segment that rewrites the whole path
///    to a target [`Identifier`].
/// 2. The current-package interpretation (`<package>.<segments…>`),
///    so user-declared types take precedence over `Global` for any
///    name they shadow.
/// 3. For multi-segment paths only: the head-as-package
///    interpretation (`<path[0]>.<path[1..]>`), so dotted names
///    like `Crypto.SHA256` resolve to the entry registered as
///    `Identifier { package: "Crypto", path: ["SHA256"] }` — i.e.
///    the same identifier the alias-rewrite path constructs from
///    `alias Crypto.SHA256 as Hasher`.
/// 4. The `Global.<segments…>` interpretation (stdlib stubs +
///    primitive types).
///
/// Multi-segment paths are accepted everywhere a single-segment
/// path is — `HTTP.Headers` resolves identically to `Headers` one
/// segment shallower, just against a different identifier shape.
pub(crate) fn resolve_path_to_global(
    path: &[String],
    span: Span,
    scope: ResolutionScope<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<GlobalRegistryId> {
    if let Some(target) = rewrite_through_aliases(scope.aliases, path) {
        if let Some((id, _)) = scope.registry.lookup(&target) {
            return Some(id);
        }
        diagnostics.push(Diagnostic::error(
            format!("typecheck does not recognize the alias target `{target}`"),
            span,
        ));
        return None;
    }
    let local = Identifier::new(scope.package, path.to_vec());
    if let Some((id, _)) = scope.registry.lookup(&local) {
        return Some(id);
    }
    if path.len() >= 2 {
        let head_as_pkg = Identifier::new(&path[0], path[1..].to_vec());
        if let Some((id, _)) = scope.registry.lookup(&head_as_pkg) {
            return Some(id);
        }
    }
    let candidate = Identifier::new("Global", path.to_vec());
    if let Some((id, entry)) = scope.registry.lookup(&candidate) {
        // Single-segment fallthrough into `Global.<name>` is reserved
        // for the stdlib primitive stubs (`Int`, `String`, …) — those
        // are the only `Global.*` entries a user-facing type
        // expression can name without qualifying further. Multi-
        // segment paths bypass this guard because the qualification
        // is the user's intent.
        if path.len() == 1 && !entry.identifier.is_in_global() {
            diagnostics.push(Diagnostic::error(
                format!(
                    "typecheck only recognizes `Global.*` primitive type names; got `{}`",
                    path[0],
                ),
                span,
            ));
            return None;
        }
        return Some(id);
    }
    diagnostics.push(Diagnostic::error(
        format!(
            "typecheck does not recognize the type name `{}` (no same-package or \
             `Global.*` entry registered)",
            path.join("."),
        ),
        span,
    ));
    None
}

pub(super) fn type_expr_span(type_expr: &TypeExpr) -> Span {
    match type_expr {
        TypeExpr::Function { span, .. }
        | TypeExpr::Generic { span, .. }
        | TypeExpr::Named { span, .. }
        | TypeExpr::Self_ { span }
        | TypeExpr::Union { span, .. }
        | TypeExpr::Unit { span } => *span,
    }
}

/// Resolve a `<T: Bound>` bound name to the protocol's registry id.
/// Lookup order matches type-name resolution: file aliases first
/// (so `<T: AliasedProtocol>` works), then `package`, then `Global`.
/// Emits a diagnostic at `span` and returns `None` when the name
/// doesn't resolve or names a non-protocol entry.
pub(crate) fn resolve_bound_to_id(
    bound: &str,
    span: Span,
    scope: ResolutionScope<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<GlobalRegistryId> {
    let path = [bound.to_string()];
    let aliased = rewrite_through_aliases(scope.aliases, &path)
        .and_then(|target| scope.registry.lookup(&target));
    let local = Identifier::new(scope.package, vec![bound.to_string()]);
    let global = Identifier::new("Global", vec![bound.to_string()]);
    let Some((id, entry)) = aliased
        .or_else(|| scope.registry.lookup(&local))
        .or_else(|| scope.registry.lookup(&global))
    else {
        diagnostics.push(Diagnostic::error(
            format!("type-parameter bound `{bound}` does not resolve to a known protocol"),
            span,
        ));
        return None;
    };
    if !matches!(entry.kind, GlobalKind::Protocol(_)) {
        diagnostics.push(Diagnostic::error(
            format!(
                "type-parameter bound `{bound}` must name a protocol (`{}` is a {})",
                entry.identifier,
                entry.kind.label(),
            ),
            span,
        ));
        return None;
    }
    Some(id)
}

pub(super) fn dispatch_label(dispatch: Dispatch) -> &'static str {
    match dispatch {
        Dispatch::Instance => "instance method (with `self`)",
        Dispatch::Static => "static method (no `self`)",
    }
}

pub(super) fn render_resolved(ty: &ResolvedType, registry: &GlobalRegistry) -> String {
    match ty {
        ResolvedType::Anonymous(AnonymousKind::Function { params, ret }) => {
            let rendered_params = params
                .iter()
                .map(|p| render_resolved(p, registry))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "fn ({rendered_params}) -> {}",
                render_resolved(ret, registry),
            )
        }
        ResolvedType::Named {
            resolution: Resolution::Global(id),
            ..
        } => match registry.get(*id) {
            Some(entry) => entry.identifier.qualified_name(),
            None => "<unknown>".to_string(),
        },
        ResolvedType::Named {
            resolution: Resolution::Local(_),
            ..
        } => "<local>".to_string(),
        ResolvedType::Named {
            resolution: Resolution::TypeParam { owner, index },
            ..
        } => registry
            .type_param_name(*owner, *index)
            .map(str::to_string)
            .unwrap_or_else(|| format!("<typeparam {owner}#{index}>")),
        ResolvedType::Named {
            resolution: Resolution::Unresolved,
            ..
        }
        | ResolvedType::Unresolved => "<unresolved>".to_string(),
        ResolvedType::Union(members) => members
            .iter()
            .map(|m| render_resolved(m, registry))
            .collect::<Vec<_>>()
            .join(" | "),
    }
}
