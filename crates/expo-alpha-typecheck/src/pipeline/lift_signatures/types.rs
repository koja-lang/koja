//! Type-expression resolution + small label/span helpers shared by
//! every other submodule under `lift_signatures/`.

use expo_ast::ast::{Diagnostic, TypeExpr};
use expo_ast::identifier::{
    GlobalRegistryId, Identifier, Resolution, ResolvedType, TypeParamIndex,
};
use expo_ast::span::Span;

use crate::registry::{Dispatch, GlobalKind, GlobalRegistry};

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
/// [`Resolution::TypeParam`]; otherwise it resolves to a preloaded
/// `Global.<name>` stub or a user struct/enum. `TypeExpr::Generic`
/// recurses into its args. `scope` is empty outside generic-decl
/// bodies (see [`TypeParamScope::default`]).
pub(crate) fn resolve_type_expr(
    type_expr: &TypeExpr,
    scope: TypeParamScope<'_>,
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    match type_expr {
        TypeExpr::Function { span, .. } => {
            diagnostics.push(Diagnostic::error(
                "alpha typecheck does not yet support function-typed annotations".to_string(),
                *span,
            ));
            ResolvedType::unresolved()
        }
        TypeExpr::Generic { path, args, span } => {
            resolve_generic(path, args, *span, scope, package, registry, diagnostics)
        }
        TypeExpr::Named { path, span } => {
            resolve_named(path, *span, scope, package, registry, diagnostics)
        }
        TypeExpr::Self_ { span } => resolve_self(*span, scope, registry, diagnostics),
        TypeExpr::Union { span, .. } => {
            diagnostics.push(Diagnostic::error(
                "alpha typecheck does not yet support union type annotations".to_string(),
                *span,
            ));
            ResolvedType::unresolved()
        }
        TypeExpr::Unit { .. } => registry.primitive("Unit"),
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
    scope: TypeParamScope<'_>,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    for &owner in scope.self_owner() {
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
            GlobalKind::Constant(_) | GlobalKind::Function(_) => continue,
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
    ResolvedType {
        resolution: Resolution::Global(owner),
        type_args,
    }
}

/// Resolve `Path<args...>`. Path resolution mirrors [`resolve_named`]
/// for the head; type args lower recursively through the same scope.
/// A type param shadows a global of the same name; `T<args>` is an
/// error because type params are arity-0.
fn resolve_generic(
    path: &[String],
    args: &[TypeExpr],
    span: Span,
    scope: TypeParamScope<'_>,
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    if path.len() == 1 && scope.lookup(&path[0], registry).is_some() {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck: type parameter `{}` cannot take type arguments",
                path[0],
            ),
            span,
        ));
        return ResolvedType::unresolved();
    }
    if path.len() != 1 {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck does not yet support dotted type names (`{}`)",
                path.join("."),
            ),
            span,
        ));
        return ResolvedType::unresolved();
    }
    let name = &path[0];
    let local = Identifier::new(package, vec![name.clone()]);
    let head = if let Some((id, _)) = registry.lookup(&local) {
        Resolution::Global(id)
    } else {
        let candidate = Identifier::new("Global", vec![name.clone()]);
        let Some((id, entry)) = registry.lookup(&candidate) else {
            diagnostics.push(Diagnostic::error(
                format!(
                    "alpha typecheck does not recognize the type name `{name}` (no \
                     same-package struct/enum or `Global.*` type registered)",
                ),
                span,
            ));
            return ResolvedType::unresolved();
        };
        if !entry.identifier.is_in_global() {
            diagnostics.push(Diagnostic::error(
                format!(
                    "alpha typecheck only recognizes `Global.*` primitive type names; \
                     got `{name}`",
                ),
                span,
            ));
            return ResolvedType::unresolved();
        }
        Resolution::Global(id)
    };
    let resolved_args = args
        .iter()
        .map(|arg| resolve_type_expr(arg, scope, package, registry, diagnostics))
        .collect();
    ResolvedType {
        resolution: head,
        type_args: resolved_args,
    }
}

fn resolve_named(
    path: &[String],
    span: Span,
    scope: TypeParamScope<'_>,
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    if path.len() != 1 {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck does not yet support dotted type names (`{}`)",
                path.join("."),
            ),
            span,
        ));
        return ResolvedType::unresolved();
    }
    let name = &path[0];
    if let Some((owner, index)) = scope.lookup(name, registry) {
        return ResolvedType::leaf(Resolution::TypeParam { owner, index });
    }
    // User-defined structs in the current package shadow stdlib
    // primitives by binding lookup order. The collect sub-pass has
    // already registered every user struct, so a same-package struct
    // entry takes precedence over a `Global.<name>` primitive.
    let local = Identifier::new(package, vec![name.clone()]);
    if let Some((id, _)) = registry.lookup(&local) {
        return ResolvedType::leaf(Resolution::Global(id));
    }
    let candidate = Identifier::new("Global", vec![name.clone()]);
    let Some((id, entry)) = registry.lookup(&candidate) else {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck does not recognize the type name `{name}` (no \
                 same-package struct or `Global.*` primitive registered)",
            ),
            span,
        ));
        return ResolvedType::unresolved();
    };
    if !entry.identifier.is_in_global() {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck only recognizes `Global.*` primitive type names; \
                 got `{name}`",
            ),
            span,
        ));
        return ResolvedType::unresolved();
    }
    ResolvedType::leaf(Resolution::Global(id))
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

/// Bare head identifier for an `impl` block's target type expression.
/// Returns `Some("Bag")` for both `impl Bag` (`TypeExpr::Named`) and
/// `impl Bag<T>` / `impl Bag<Int>` (`TypeExpr::Generic`); `None` for
/// multi-segment paths and non-`Named`/`Generic` shapes. Methods on
/// the impl block register under this head name — `collect`, `lift`,
/// and `resolve` all share the same key.
///
/// `pub(crate)` so the resolve walker reuses the same shape match
/// when stamping per-method `LocalId`s.
pub(crate) fn impl_target_name(target: &TypeExpr) -> Option<&str> {
    match target {
        TypeExpr::Named { path, .. } | TypeExpr::Generic { path, .. } if path.len() == 1 => {
            Some(path[0].as_str())
        }
        _ => None,
    }
}

/// Resolve a `<T: Bound>` bound name to the protocol's registry id.
/// Looks up `bound` first in `package` then under `Global`. Emits a
/// diagnostic at `span` and returns `None` when the name doesn't
/// resolve or names a non-protocol entry.
pub(crate) fn resolve_bound_to_id(
    bound: &str,
    span: Span,
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<GlobalRegistryId> {
    let local = Identifier::new(package, vec![bound.to_string()]);
    let global = Identifier::new("Global", vec![bound.to_string()]);
    let Some((id, entry)) = registry.lookup(&local).or_else(|| registry.lookup(&global)) else {
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
    match ty.resolution {
        Resolution::Global(id) => match registry.get(id) {
            Some(entry) => entry.identifier.qualified_name(),
            None => "<unknown>".to_string(),
        },
        Resolution::Local(_) => "<local>".to_string(),
        Resolution::TypeParam { owner, index } => registry
            .type_param_name(owner, index)
            .map(str::to_string)
            .unwrap_or_else(|| format!("<typeparam {owner}#{index}>")),
        Resolution::Unresolved => "<unresolved>".to_string(),
    }
}
